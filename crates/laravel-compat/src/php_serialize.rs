//! PHP `serialize()` / `unserialize()` for scalars and arrays.
//! Objects and references are rejected (never produced for panel data).

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum PhpKey {
    Int(i64),
    Str(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum PhpValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(Vec<u8>),
    /// Ordered PHP array.
    Array(Vec<(PhpKey, PhpValue)>),
}

impl PhpValue {
    pub fn string(s: impl Into<String>) -> Self {
        PhpValue::String(s.into().into_bytes())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            PhpValue::String(b) => std::str::from_utf8(b).ok(),
            _ => None,
        }
    }

    /// Lookup by string key (or by an integer key given as decimal string, like PHP).
    pub fn get(&self, key: &str) -> Option<&PhpValue> {
        let PhpValue::Array(items) = self else {
            return None;
        };
        items.iter().find_map(|(k, v)| match k {
            PhpKey::Str(s) if s == key.as_bytes() => Some(v),
            PhpKey::Int(i) if i.to_string() == key => Some(v),
            _ => None,
        })
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unserialize error at offset {offset}: {msg}")]
pub struct UnserializeError {
    pub offset: usize,
    pub msg: &'static str,
}

pub fn serialize(v: &PhpValue) -> Vec<u8> {
    let mut out = Vec::new();
    write_value(&mut out, v);
    out
}

fn write_value(out: &mut Vec<u8>, v: &PhpValue) {
    match v {
        PhpValue::Null => out.extend_from_slice(b"N;"),
        PhpValue::Bool(b) => out.extend_from_slice(if *b { b"b:1;" } else { b"b:0;" }),
        PhpValue::Int(i) => out.extend_from_slice(format!("i:{i};").as_bytes()),
        PhpValue::Float(f) => out.extend_from_slice(format!("d:{};", PhpFloat(*f)).as_bytes()),
        PhpValue::String(s) => {
            out.extend_from_slice(format!("s:{}:\"", s.len()).as_bytes());
            out.extend_from_slice(s);
            out.extend_from_slice(b"\";");
        }
        PhpValue::Array(items) => {
            out.extend_from_slice(format!("a:{}:{{", items.len()).as_bytes());
            for (k, v) in items {
                match k {
                    PhpKey::Int(i) => write_value(out, &PhpValue::Int(*i)),
                    PhpKey::Str(s) => write_value(out, &PhpValue::String(s.clone())),
                }
                write_value(out, v);
            }
            out.push(b'}');
        }
    }
}

/// PHP float formatting with `serialize_precision = -1` (shortest round-trip).
pub struct PhpFloat(pub f64);

impl fmt::Display for PhpFloat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = self.0;
        if v.is_nan() {
            return f.write_str("NAN");
        }
        if v.is_infinite() {
            return f.write_str(if v > 0.0 { "INF" } else { "-INF" });
        }
        // Rust's `{}` for f64 is shortest round-trip but never uses exponents;
        // PHP (zend_gcvt, precision 17) switches to exponent form when decpt < -3 or > 17.
        let abs = v.abs();
        if v != 0.0 && !(1e-4..1e17).contains(&abs) {
            let s = format!("{v:e}"); // e.g. 1.5e20, 1e-7
            let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
            let mant = if mant.contains('.') {
                mant.to_string()
            } else {
                format!("{mant}.0")
            };
            let exp: i32 = exp.parse().unwrap_or(0);
            return write!(f, "{mant}E{}{}", if exp < 0 { '-' } else { '+' }, exp.abs());
        }
        let s = format!("{v}");
        f.write_str(&s)
    }
}

pub fn unserialize(input: &[u8]) -> Result<PhpValue, UnserializeError> {
    let mut p = Parser { s: input, i: 0 };
    let v = p.value(0)?;
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn err(&self, msg: &'static str) -> UnserializeError {
        UnserializeError {
            offset: self.i,
            msg,
        }
    }

    fn expect(&mut self, b: u8) -> Result<(), UnserializeError> {
        if self.s.get(self.i) == Some(&b) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.err("unexpected byte"))
        }
    }

    fn until(&mut self, end: u8) -> Result<&[u8], UnserializeError> {
        let start = self.i;
        while self.i < self.s.len() && self.s[self.i] != end {
            self.i += 1;
        }
        if self.i >= self.s.len() {
            return Err(self.err("unterminated token"));
        }
        let out = &self.s[start..self.i];
        self.i += 1;
        Ok(out)
    }

    fn int_until(&mut self, end: u8) -> Result<i64, UnserializeError> {
        let raw = self.until(end)?;
        std::str::from_utf8(raw)
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(UnserializeError {
                offset: self.i,
                msg: "invalid integer",
            })
    }

    fn value(&mut self, depth: usize) -> Result<PhpValue, UnserializeError> {
        if depth > 64 {
            return Err(self.err("nesting too deep"));
        }
        let tag = *self
            .s
            .get(self.i)
            .ok_or_else(|| self.err("unexpected end"))?;
        self.i += 1;
        match tag {
            b'N' => {
                self.expect(b';')?;
                Ok(PhpValue::Null)
            }
            b'b' => {
                self.expect(b':')?;
                Ok(PhpValue::Bool(self.int_until(b';')? != 0))
            }
            b'i' => {
                self.expect(b':')?;
                Ok(PhpValue::Int(self.int_until(b';')?))
            }
            b'd' => {
                self.expect(b':')?;
                let raw = self.until(b';')?.to_vec();
                let parsed = match std::str::from_utf8(&raw) {
                    Ok("NAN") => Some(f64::NAN),
                    Ok("INF") => Some(f64::INFINITY),
                    Ok("-INF") => Some(f64::NEG_INFINITY),
                    Ok(s) => s.parse().ok(),
                    Err(_) => None,
                };
                let f = parsed.ok_or_else(|| self.err("invalid float"))?;
                Ok(PhpValue::Float(f))
            }
            b's' => {
                self.expect(b':')?;
                let len = usize::try_from(self.int_until(b':')?)
                    .map_err(|_| self.err("negative length"))?;
                self.expect(b'"')?;
                let end = self
                    .i
                    .checked_add(len)
                    .filter(|e| *e <= self.s.len())
                    .ok_or_else(|| self.err("string too long"))?;
                let bytes = self.s[self.i..end].to_vec();
                self.i = end;
                self.expect(b'"')?;
                self.expect(b';')?;
                Ok(PhpValue::String(bytes))
            }
            b'a' => {
                self.expect(b':')?;
                let n = usize::try_from(self.int_until(b':')?)
                    .map_err(|_| self.err("negative length"))?;
                self.expect(b'{')?;
                let mut items = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    let key = match self.value(depth + 1)? {
                        PhpValue::Int(i) => PhpKey::Int(i),
                        PhpValue::String(s) => PhpKey::Str(s),
                        _ => return Err(self.err("invalid array key")),
                    };
                    let v = self.value(depth + 1)?;
                    items.push((key, v));
                }
                self.expect(b'}')?;
                Ok(PhpValue::Array(items))
            }
            _ => Err(self.err("unsupported type")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn php_fixture_roundtrip() {
        let v = unserialize(crate::fixtures::SERIALIZED).unwrap();
        assert_eq!(v.get("a"), Some(&PhpValue::Int(1)));
        assert_eq!(v.get("u").and_then(|u| u.as_str()), Some("ü"));
        assert_eq!(serialize(&v), crate::fixtures::SERIALIZED);
    }

    #[test]
    fn floats() {
        assert_eq!(PhpFloat(1.5).to_string(), "1.5");
        assert_eq!(PhpFloat(1.0).to_string(), "1");
        assert_eq!(PhpFloat(0.1).to_string(), "0.1");
        assert_eq!(PhpFloat(1e20).to_string(), "1.0E+20");
        assert_eq!(PhpFloat(1e-5).to_string(), "1.0E-5");
        assert_eq!(PhpFloat(1e15).to_string(), "1000000000000000");
        assert_eq!(PhpFloat(-0.0).to_string(), "-0");
    }
}
