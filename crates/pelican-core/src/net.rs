//! Client IP resolution (Symfony `Request::getClientIp` + Laravel TrustProxies)
//! and `IPTools\Range` containment used by API key `allowed_ips`.

use std::net::IpAddr;

/// One `TRUSTED_PROXIES` entry or allowed-ip network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IpRange {
    first: IpAddr,
    last: IpAddr,
}

fn octets(ip: &IpAddr) -> Vec<u8> {
    match ip {
        IpAddr::V4(v) => v.octets().to_vec(),
        IpAddr::V6(v) => v.octets().to_vec(),
    }
}

fn from_bytes(b: &[u8]) -> Option<IpAddr> {
    match b.len() {
        4 => Some(IpAddr::from(<[u8; 4]>::try_from(b).ok()?)),
        16 => Some(IpAddr::from(<[u8; 16]>::try_from(b).ok()?)),
        _ => None,
    }
}

fn network(ip: IpAddr, prefix: u32) -> Option<IpRange> {
    let mut lo = octets(&ip);
    let bits = (lo.len() * 8) as u32;
    if prefix > bits {
        return None;
    }
    let mut hi = lo.clone();
    for i in 0..bits {
        if i >= prefix {
            let (byte, bit) = ((i / 8) as usize, 7 - (i % 8));
            lo[byte] &= !(1 << bit);
            hi[byte] |= 1 << bit;
        }
    }
    Some(IpRange {
        first: from_bytes(&lo)?,
        last: from_bytes(&hi)?,
    })
}

fn mask_prefix(mask: IpAddr) -> Option<u32> {
    let b = octets(&mask);
    let ones: u32 = b.iter().map(|x| x.count_ones()).sum();
    // Must be a contiguous mask.
    let expect = network(mask, ones).map(|r| octets(&r.first));
    (expect.as_deref() == Some(b.as_slice())).then_some(ones)
}

impl IpRange {
    /// `IPTools\Range::parse`: CIDR / "ip mask", wildcard (`*`), `a-b`, single IP.
    pub fn parse(s: &str) -> Option<IpRange> {
        let s = s.trim();
        if s.contains('/') || s.contains(' ') {
            let (ip, rest) = s.split_once('/').or_else(|| s.split_once(' '))?;
            let ip: IpAddr = ip.trim().parse().ok()?;
            let rest = rest.trim();
            let prefix = match rest.parse::<u32>() {
                Ok(p) => p,
                Err(_) => mask_prefix(rest.parse().ok()?)?,
            };
            return network(ip, prefix);
        }
        if s.contains('*') {
            let first: IpAddr = s.replace('*', "0").parse().ok()?;
            let last: IpAddr = s.replace('*', "255").parse().ok()?;
            return Some(IpRange { first, last });
        }
        if let Some(pos) = s.find('-').filter(|p| *p > 0) {
            let first: IpAddr = s[..pos].trim().parse().ok()?;
            let last: IpAddr = s[pos + 1..].trim().parse().ok()?;
            return Some(IpRange { first, last });
        }
        let ip: IpAddr = s.parse().ok()?;
        Some(IpRange {
            first: ip,
            last: ip,
        })
    }

    /// Byte-wise comparison of packed addresses (v4 never matches v6).
    pub fn contains(&self, ip: &IpAddr) -> bool {
        let (f, l, x) = (octets(&self.first), octets(&self.last), octets(ip));
        f.len() == x.len() && l.len() == x.len() && f <= x && x <= l
    }
}

/// Resolve the client IP like Laravel/Symfony with `TRUSTED_PROXIES`.
pub fn client_ip(remote: IpAddr, forwarded_for: Option<&str>, trusted: &[String]) -> IpAddr {
    let is_trusted = |ip: &IpAddr| -> bool {
        trusted.iter().any(|t| match t.as_str() {
            "**" => true,
            "*" => ip == &remote,
            other => IpRange::parse(other).is_some_and(|r| r.contains(ip)),
        })
    };
    if trusted.is_empty() || !is_trusted(&remote) {
        return remote;
    }
    let mut chain: Vec<IpAddr> = forwarded_for
        .unwrap_or("")
        .split(',')
        .filter_map(|p| {
            let p = p.trim();
            // Strip an optional port.
            p.parse::<IpAddr>().ok().or_else(|| {
                p.rsplit_once(':')
                    .and_then(|(h, _)| h.trim_matches(['[', ']']).parse().ok())
            })
        })
        .collect();
    chain.push(remote);
    let mut first_trusted = None;
    let untrusted: Vec<IpAddr> = chain
        .into_iter()
        .filter(|ip| {
            if is_trusted(ip) {
                first_trusted.get_or_insert(*ip);
                false
            } else {
                true
            }
        })
        .collect();
    untrusted
        .last()
        .copied()
        .or(first_trusted)
        .unwrap_or(remote)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        let r = IpRange::parse("192.168.1.0/24").unwrap();
        assert!(r.contains(&"192.168.1.77".parse().unwrap()));
        assert!(!r.contains(&"192.168.2.1".parse().unwrap()));
        assert!(
            IpRange::parse("10.0.0.0 255.0.0.0")
                .unwrap()
                .contains(&"10.9.9.9".parse().unwrap())
        );
        assert!(
            IpRange::parse("10.1.*.*")
                .unwrap()
                .contains(&"10.1.200.3".parse().unwrap())
        );
        assert!(
            IpRange::parse("10.0.0.5-10.0.0.9")
                .unwrap()
                .contains(&"10.0.0.7".parse().unwrap())
        );
        assert!(
            IpRange::parse("::1")
                .unwrap()
                .contains(&"::1".parse().unwrap())
        );
        assert!(
            !IpRange::parse("127.0.0.1")
                .unwrap()
                .contains(&"::1".parse().unwrap())
        );
        assert!(IpRange::parse("nope").is_none());
    }

    #[test]
    fn forwarded() {
        let t = vec!["0.0.0.0/0".to_string()];
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(
            client_ip(lo, Some("10.0.0.1"), &t),
            "10.0.0.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            client_ip(lo, Some("10.0.0.1, 172.18.0.5"), &t),
            "10.0.0.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            client_ip(lo, Some("2001:db8::1"), &t),
            "2001:db8::1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(client_ip(lo, Some("10.0.0.1"), &[]), lo);
        assert_eq!(
            client_ip(lo, Some("10.0.0.1, 8.8.8.8"), &["*".into()]),
            "8.8.8.8".parse::<IpAddr>().unwrap()
        );
    }
}
