//! Public SSH keys as phpseclib's `PublicKeyLoader` handles them for the panel:
//! parse (OpenSSH, SPKI PEM, PKCS#1 RSA PEM), `toString('PKCS8')`,
//! `getFingerprint('sha256')` (client-api.md §8.2, remote-api.md §2.1).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Rsa {
        n: Vec<u8>,
        e: Vec<u8>,
    },
    Ed25519 {
        key: Vec<u8>,
    },
    /// curve: `nistp256` | `nistp384` | `nistp521`; point: SEC1 encoded.
    Ecdsa {
        curve: String,
        point: Vec<u8>,
    },
    Dsa,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey {
    pub kind: KeyKind,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("no key loaded")]
pub struct NoKeyLoaded;

fn read_string(b: &[u8], pos: &mut usize) -> Option<Vec<u8>> {
    let len = u32::from_be_bytes(b.get(*pos..*pos + 4)?.try_into().ok()?) as usize;
    *pos += 4;
    let v = b.get(*pos..*pos + len)?.to_vec();
    *pos += len;
    Some(v)
}

fn write_string(out: &mut Vec<u8>, v: &[u8]) {
    out.extend_from_slice(&(v.len() as u32).to_be_bytes());
    out.extend_from_slice(v);
}

/// SSH mpint: strip leading zeros, add one back if the high bit is set.
fn mpint(v: &[u8]) -> Vec<u8> {
    let mut s: &[u8] = v;
    while s.len() > 1 && s[0] == 0 {
        s = &s[1..];
    }
    let mut out = Vec::with_capacity(s.len() + 1);
    if s.first().is_some_and(|b| b & 0x80 != 0) {
        out.push(0);
    }
    out.extend_from_slice(s);
    out
}

fn strip_zeros(v: &[u8]) -> Vec<u8> {
    let mut s: &[u8] = v;
    while s.len() > 1 && s[0] == 0 {
        s = &s[1..];
    }
    s.to_vec()
}

fn parse_openssh_blob(blob: &[u8]) -> Option<KeyKind> {
    let mut p = 0;
    let kind = read_string(blob, &mut p)?;
    match kind.as_slice() {
        b"ssh-rsa" => {
            let e = read_string(blob, &mut p)?;
            let n = read_string(blob, &mut p)?;
            Some(KeyKind::Rsa { n: strip_zeros(&n), e: strip_zeros(&e) })
        }
        b"ssh-ed25519" => {
            let key = read_string(blob, &mut p)?;
            (key.len() == 32).then_some(KeyKind::Ed25519 { key })
        }
        b"ssh-dss" => Some(KeyKind::Dsa),
        k if k.starts_with(b"ecdsa-sha2-") => {
            let curve = String::from_utf8(read_string(blob, &mut p)?).ok()?;
            let point = read_string(blob, &mut p)?;
            matches!(curve.as_str(), "nistp256" | "nistp384" | "nistp521").then_some(KeyKind::Ecdsa { curve, point })
        }
        _ => None,
    }
}

// --- minimal DER ---------------------------------------------------------

fn der_len(len: usize) -> Vec<u8> {
    if len < 0x80 {
        vec![len as u8]
    } else {
        let bytes: Vec<u8> = len.to_be_bytes().iter().copied().skip_while(|b| *b == 0).collect();
        let mut out = vec![0x80 | bytes.len() as u8];
        out.extend(bytes);
        out
    }
}

fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend(der_len(content.len()));
    out.extend_from_slice(content);
    out
}

fn der_int(v: &[u8]) -> Vec<u8> {
    der(0x02, &mpint(v))
}

fn der_read(b: &[u8], pos: &mut usize) -> Option<(u8, Vec<u8>)> {
    let tag = *b.get(*pos)?;
    *pos += 1;
    let first = *b.get(*pos)?;
    *pos += 1;
    let len = if first & 0x80 == 0 {
        first as usize
    } else {
        let n = (first & 0x7f) as usize;
        let mut l = 0usize;
        for _ in 0..n {
            l = (l << 8) | *b.get(*pos)? as usize;
            *pos += 1;
        }
        l
    };
    let v = b.get(*pos..*pos + len)?.to_vec();
    *pos += len;
    Some((tag, v))
}

const OID_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const OID_EC: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_ED25519: &[u8] = &[0x2b, 0x65, 0x70];
const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const OID_P384: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];
const OID_P521: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x23];

fn parse_pkcs1_rsa(der_bytes: &[u8]) -> Option<KeyKind> {
    let mut p = 0;
    let (tag, seq) = der_read(der_bytes, &mut p)?;
    if tag != 0x30 {
        return None;
    }
    let mut q = 0;
    let (_, n) = der_read(&seq, &mut q)?;
    let (_, e) = der_read(&seq, &mut q)?;
    Some(KeyKind::Rsa { n: strip_zeros(&n), e: strip_zeros(&e) })
}

fn parse_spki(der_bytes: &[u8]) -> Option<KeyKind> {
    let mut p = 0;
    let (_, spki) = der_read(der_bytes, &mut p)?;
    let mut q = 0;
    let (_, alg) = der_read(&spki, &mut q)?;
    let (_, bits) = der_read(&spki, &mut q)?;
    let key = bits.get(1..)?.to_vec(); // skip "unused bits" byte
    let mut a = 0;
    let (_, oid) = der_read(&alg, &mut a)?;
    match oid.as_slice() {
        OID_RSA => parse_pkcs1_rsa(&key),
        OID_ED25519 => (key.len() == 32).then_some(KeyKind::Ed25519 { key }),
        OID_EC => {
            let (_, curve_oid) = der_read(&alg, &mut a)?;
            let curve = match curve_oid.as_slice() {
                OID_P256 => "nistp256",
                OID_P384 => "nistp384",
                OID_P521 => "nistp521",
                _ => return None,
            };
            Some(KeyKind::Ecdsa { curve: curve.into(), point: key })
        }
        _ => None,
    }
}

fn pem_body(s: &str, label: &str) -> Option<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = s.find(&begin)? + begin.len();
    let stop = s[start..].find(&end)? + start;
    let b64: String = s[start..stop].chars().filter(|c| !c.is_whitespace()).collect();
    STANDARD.decode(b64).ok()
}

impl PublicKey {
    /// `PublicKeyLoader::loadPublicKey($input)` (private keys are rejected).
    pub fn parse(input: &str) -> Result<PublicKey, NoKeyLoaded> {
        let s = input.trim();
        if s.contains("PRIVATE KEY") {
            return Err(NoKeyLoaded);
        }
        let kind = if let Some(d) = pem_body(s, "PUBLIC KEY") {
            parse_spki(&d)
        } else if let Some(d) = pem_body(s, "RSA PUBLIC KEY") {
            parse_pkcs1_rsa(&d)
        } else {
            let mut parts = s.split_whitespace();
            let first = parts.next().unwrap_or("");
            let b64 = if first.starts_with("ssh-") || first.starts_with("ecdsa-") { parts.next() } else { Some(first) };
            b64.and_then(|b| STANDARD.decode(b).ok()).and_then(|blob| parse_openssh_blob(&blob))
        };
        kind.map(|kind| PublicKey { kind }).ok_or(NoKeyLoaded)
    }

    /// OpenSSH wire blob (what the fingerprint hashes).
    pub fn openssh_blob(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match &self.kind {
            KeyKind::Rsa { n, e } => {
                write_string(&mut out, b"ssh-rsa");
                write_string(&mut out, &mpint(e));
                write_string(&mut out, &mpint(n));
            }
            KeyKind::Ed25519 { key } => {
                write_string(&mut out, b"ssh-ed25519");
                write_string(&mut out, key);
            }
            KeyKind::Ecdsa { curve, point } => {
                write_string(&mut out, format!("ecdsa-sha2-{curve}").as_bytes());
                write_string(&mut out, curve.as_bytes());
                write_string(&mut out, point);
            }
            KeyKind::Dsa => {}
        }
        out
    }

    /// `getFingerprint('sha256')`: base64 of SHA-256 with the trailing `=` removed.
    pub fn fingerprint_sha256(&self) -> String {
        STANDARD.encode(Sha256::digest(self.openssh_blob())).trim_end_matches('=').to_string()
    }

    /// RSA modulus length in bits (`getLength()`).
    pub fn rsa_bits(&self) -> Option<usize> {
        match &self.kind {
            KeyKind::Rsa { n, .. } => {
                let n = strip_zeros(n);
                Some(n.len() * 8 - n.first().map(|b| b.leading_zeros() as usize).unwrap_or(0))
            }
            _ => None,
        }
    }

    /// `toString('PKCS8')`: SPKI PEM with `\r\n` line endings and 64-char lines.
    pub fn to_pkcs8_pem(&self) -> String {
        let spki = match &self.kind {
            KeyKind::Rsa { n, e } => {
                let pkcs1 = der(0x30, &[der_int(n), der_int(e)].concat());
                let alg = der(0x30, &[der(0x06, OID_RSA), der(0x05, &[])].concat());
                der(0x30, &[alg, der(0x03, &[&[0u8][..], &pkcs1].concat())].concat())
            }
            KeyKind::Ed25519 { key } => {
                let alg = der(0x30, &der(0x06, OID_ED25519));
                der(0x30, &[alg, der(0x03, &[&[0u8][..], key].concat())].concat())
            }
            KeyKind::Ecdsa { curve, point } => {
                let curve_oid = match curve.as_str() {
                    "nistp256" => OID_P256,
                    "nistp384" => OID_P384,
                    _ => OID_P521,
                };
                let alg = der(0x30, &[der(0x06, OID_EC), der(0x06, curve_oid)].concat());
                der(0x30, &[alg, der(0x03, &[&[0u8][..], point].concat())].concat())
            }
            KeyKind::Dsa => Vec::new(),
        };
        let b64 = STANDARD.encode(spki);
        let mut out = String::from("-----BEGIN PUBLIC KEY-----\r\n");
        for chunk in b64.as_bytes().chunks(64) {
            out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
            out.push_str("\r\n");
        }
        out.push_str("-----END PUBLIC KEY-----");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ed25519_fingerprint() {
        let k = PublicKey::parse(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINY/Gpl4X9rAo73ku0nIm3OdxyGDAme+wYNeSj2WDR58 difftest",
        )
        .unwrap();
        assert_eq!(k.fingerprint_sha256(), "QfiNrXNQO28JKVd5iXnW/mBHGYgay9sfXT6QJSAgisg");
        let pem = k.to_pkcs8_pem();
        assert!(pem.starts_with("-----BEGIN PUBLIC KEY-----\r\nMCowBQYDK2VwAyEA"));
        assert_eq!(PublicKey::parse(&pem).unwrap(), k);
        assert!(PublicKey::parse("nonsense").is_err());
    }
}
