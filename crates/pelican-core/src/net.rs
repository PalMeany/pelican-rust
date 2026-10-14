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
    Some(IpRange { first: from_bytes(&lo)?, last: from_bytes(&hi)? })
}

fn mask_prefix(mask: IpAddr) -> Option<u32> {
    let b = octets(&mask);
    let ones: u32 = b.iter().map(|x| x.count_ones()).sum();
    // Must be a contiguous mask.
    let expect = network(mask, ones).map(|r| octets(&r.first));
    (expect.as_deref() == Some(b.as_slice())).then_some(ones)
}

/// Failure of `Range::parse` (IPTools throws).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpParseError {
    /// `IpException("Invalid IP address format")` — callers treat this as "not valid".
    InvalidFormat,
    /// Any other IPTools exception (bad hex/binary, prefix, netmask…) — rethrown as 500.
    Other(&'static str),
}

/// `IP::parse()`: `0x` hex, `0b` binary, numeric → `long2ip`, else FILTER_VALIDATE_IP.
fn ip_parse(s: &str) -> Result<IpAddr, IpParseError> {
    if let Some(hex) = s.strip_prefix("0x") {
        let ok = (hex.len() == 8 || hex.len() == 32) && hex.chars().all(|c| c.is_ascii_hexdigit());
        if !ok {
            return Err(IpParseError::Other("Invalid hexadecimal IP address format"));
        }
        let bytes: Vec<u8> =
            (0..hex.len()).step_by(2).filter_map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok()).collect();
        return from_bytes(&bytes).ok_or(IpParseError::Other("Invalid hexadecimal IP address format"));
    }
    if let Some(bin) = s.strip_prefix("0b") {
        let ok = (bin.len() == 32 || bin.len() == 128) && bin.chars().all(|c| c == '0' || c == '1');
        if !ok {
            return Err(IpParseError::Other("Invalid binary IP address format"));
        }
        let bytes: Vec<u8> =
            (0..bin.len()).step_by(8).filter_map(|i| u8::from_str_radix(&bin[i..i + 8], 2).ok()).collect();
        return from_bytes(&bytes).ok_or(IpParseError::Other("Invalid binary IP address format"));
    }
    if crate::php::is_numeric_str(s) {
        // long2ip((int) $s): the low 32 bits.
        let n = s.trim().parse::<f64>().unwrap_or(0.0) as i64;
        let v = (n as u64 & 0xffff_ffff) as u32;
        return Ok(IpAddr::from(v.to_be_bytes()));
    }
    s.parse::<IpAddr>().map_err(|_| IpParseError::InvalidFormat)
}

fn prefix_of(mask: &IpAddr) -> Result<u32, IpParseError> {
    mask_prefix(*mask).ok_or(IpParseError::Other("Invalid Netmask address format"))
}

impl IpRange {
    /// `IPTools\Range::parse`: CIDR / "ip mask", wildcard (`*`), `a-b`, single IP.
    /// Note PHP `strpos(...)` truthiness: a `/`, ` ` or `-` at position 0 does not count.
    pub fn try_parse(data: &str) -> Result<IpRange, IpParseError> {
        let pos = |c: char| data.find(c).filter(|p| *p > 0);
        if pos('/').is_some() || pos(' ').is_some() {
            // Network::parse
            if let Some((ip, prefix)) =
                data.rsplit_once('/').filter(|(_, p)| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            {
                let ip = ip_parse(ip)?;
                let max = if ip.is_ipv4() { 32 } else { 128 };
                let prefix: u32 = prefix.parse().map_err(|_| IpParseError::Other("Invalid prefix length"))?;
                if prefix > max {
                    return Err(IpParseError::Other("Invalid prefix length"));
                }
                return network(ip, prefix).ok_or(IpParseError::Other("Invalid prefix length"));
            }
            if let Some((ip, mask)) = data.split_once(' ').filter(|_| pos(' ').is_some()) {
                let ip = ip_parse(ip)?;
                let mask = ip_parse(mask)?;
                return network(ip, prefix_of(&mask)?).ok_or(IpParseError::Other("Invalid Netmask address format"));
            }
            let ip = ip_parse(data)?;
            return Ok(IpRange { first: ip, last: ip });
        }
        if data.contains('*') {
            let first = ip_parse(&data.replace('*', "0"))?;
            let last = ip_parse(&data.replace('*', "255"))?;
            return Ok(IpRange { first, last });
        }
        if let Some(p) = pos('-') {
            let first = ip_parse(&data[..p])?;
            let last = ip_parse(&data[p + 1..])?;
            return Ok(IpRange { first, last });
        }
        let ip = ip_parse(data)?;
        Ok(IpRange { first: ip, last: ip })
    }

    /// Convenience: `None` for any parse failure.
    pub fn parse(s: &str) -> Option<IpRange> {
        Self::try_parse(s.trim()).ok()
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
            p.parse::<IpAddr>()
                .ok()
                .or_else(|| p.rsplit_once(':').and_then(|(h, _)| h.trim_matches(['[', ']']).parse().ok()))
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
    untrusted.last().copied().or(first_trusted).unwrap_or(remote)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        let r = IpRange::parse("192.168.1.0/24").unwrap();
        assert!(r.contains(&"192.168.1.77".parse().unwrap()));
        assert!(!r.contains(&"192.168.2.1".parse().unwrap()));
        assert!(IpRange::parse("10.0.0.0 255.0.0.0").unwrap().contains(&"10.9.9.9".parse().unwrap()));
        assert!(IpRange::parse("10.1.*.*").unwrap().contains(&"10.1.200.3".parse().unwrap()));
        assert!(IpRange::parse("10.0.0.5-10.0.0.9").unwrap().contains(&"10.0.0.7".parse().unwrap()));
        assert!(IpRange::parse("::1").unwrap().contains(&"::1".parse().unwrap()));
        assert!(!IpRange::parse("127.0.0.1").unwrap().contains(&"::1".parse().unwrap()));
        assert!(IpRange::parse("nope").is_none());
        assert_eq!(IpRange::try_parse("nope"), Err(IpParseError::InvalidFormat));
        assert!(IpRange::parse("5").unwrap().contains(&"0.0.0.5".parse().unwrap()));
        assert!(matches!(IpRange::try_parse("0xzz"), Err(IpParseError::Other(_))));
    }

    #[test]
    fn forwarded() {
        let t = vec!["0.0.0.0/0".to_string()];
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(client_ip(lo, Some("10.0.0.1"), &t), "10.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(client_ip(lo, Some("10.0.0.1, 172.18.0.5"), &t), "10.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(client_ip(lo, Some("2001:db8::1"), &t), "2001:db8::1".parse::<IpAddr>().unwrap());
        assert_eq!(client_ip(lo, Some("10.0.0.1"), &[]), lo);
        assert_eq!(client_ip(lo, Some("10.0.0.1, 8.8.8.8"), &["*".into()]), "8.8.8.8".parse::<IpAddr>().unwrap());
    }
}
