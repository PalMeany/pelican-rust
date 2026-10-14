//! PHP `json_encode()` compatible serialization (ext/json/json_encoder.c).
//!
//! Defaults (flags = 0): `/` → `\/`, non-ASCII → `\uXXXX` (lowercase hex,
//! UTF-16 surrogate pairs), control chars → `\b \f \n \r \t` or `\u00xx`,
//! floats with `serialize_precision = -1` and without `.0` for integral values.
//! Key order is preserved (serde_json `preserve_order`).

use std::io;

use serde::Serialize;
use serde_json::ser::{CharEscape, CompactFormatter, Formatter, PrettyFormatter};

pub const JSON_HEX_TAG: u32 = 1;
pub const JSON_HEX_AMP: u32 = 2;
pub const JSON_HEX_APOS: u32 = 4;
pub const JSON_HEX_QUOT: u32 = 8;
pub const JSON_PRETTY_PRINT: u32 = 128;
pub const JSON_UNESCAPED_SLASHES: u32 = 64;
pub const JSON_UNESCAPED_UNICODE: u32 = 256;
pub const JSON_PRESERVE_ZERO_FRACTION: u32 = 1024;
pub const JSON_UNESCAPED_LINE_TERMINATORS: u32 = 2048;

struct PhpFormatter<F> {
    inner: F,
    flags: u32,
}

const HEX: &[u8; 16] = b"0123456789abcdef";

impl<F: Formatter> PhpFormatter<F> {
    fn has(&self, f: u32) -> bool {
        self.flags & f != 0
    }

    fn write_u16_escape<W: ?Sized + io::Write>(w: &mut W, u: u16) -> io::Result<()> {
        let buf = [
            b'\\',
            b'u',
            HEX[(u >> 12) as usize & 0xf],
            HEX[(u >> 8) as usize & 0xf],
            HEX[(u >> 4) as usize & 0xf],
            HEX[u as usize & 0xf],
        ];
        w.write_all(&buf)
    }
}

impl<F: Formatter> Formatter for PhpFormatter<F> {
    fn write_f64<W: ?Sized + io::Write>(&mut self, w: &mut W, value: f64) -> io::Result<()> {
        w.write_all(php_json_float(value, self.has(JSON_PRESERVE_ZERO_FRACTION)).as_bytes())
    }

    fn write_f32<W: ?Sized + io::Write>(&mut self, w: &mut W, value: f32) -> io::Result<()> {
        self.write_f64(w, f64::from(value))
    }

    fn write_string_fragment<W: ?Sized + io::Write>(&mut self, w: &mut W, fragment: &str) -> io::Result<()> {
        let mut start = 0;
        for (i, ch) in fragment.char_indices() {
            let esc: Option<&[u8]> = match ch {
                '/' if !self.has(JSON_UNESCAPED_SLASHES) => Some(b"\\/"),
                '<' if self.has(JSON_HEX_TAG) => Some(b"\\u003C"),
                '>' if self.has(JSON_HEX_TAG) => Some(b"\\u003E"),
                '&' if self.has(JSON_HEX_AMP) => Some(b"\\u0026"),
                '\'' if self.has(JSON_HEX_APOS) => Some(b"\\u0027"),
                _ => None,
            };
            let needs_unicode_escape = (ch as u32) >= 0x80
                && (!self.has(JSON_UNESCAPED_UNICODE)
                    || ((ch == '\u{2028}' || ch == '\u{2029}') && !self.has(JSON_UNESCAPED_LINE_TERMINATORS)));
            if esc.is_none() && !needs_unicode_escape {
                continue;
            }
            w.write_all(&fragment.as_bytes()[start..i])?;
            if let Some(e) = esc {
                w.write_all(e)?;
            } else {
                let mut buf = [0u16; 2];
                for u in ch.encode_utf16(&mut buf) {
                    Self::write_u16_escape(w, *u)?;
                }
            }
            start = i + ch.len_utf8();
        }
        w.write_all(&fragment.as_bytes()[start..])
    }

    fn write_char_escape<W: ?Sized + io::Write>(&mut self, w: &mut W, escape: CharEscape) -> io::Result<()> {
        match escape {
            CharEscape::Quote if self.has(JSON_HEX_QUOT) => w.write_all(b"\\u0022"),
            CharEscape::Quote => w.write_all(b"\\\""),
            CharEscape::ReverseSolidus => w.write_all(b"\\\\"),
            CharEscape::Backspace => w.write_all(b"\\b"),
            CharEscape::FormFeed => w.write_all(b"\\f"),
            CharEscape::LineFeed => w.write_all(b"\\n"),
            CharEscape::CarriageReturn => w.write_all(b"\\r"),
            CharEscape::Tab => w.write_all(b"\\t"),
            CharEscape::AsciiControl(b) => Self::write_u16_escape(w, u16::from(b)),
            #[allow(unreachable_patterns)]
            _ => self.inner.write_char_escape(w, escape),
        }
    }

    // Structural output is delegated (compact or 4-space pretty, like PHP).
    fn begin_array<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.begin_array(w)
    }
    fn end_array<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.end_array(w)
    }
    fn begin_array_value<W: ?Sized + io::Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
        self.inner.begin_array_value(w, first)
    }
    fn end_array_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.end_array_value(w)
    }
    fn begin_object<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.begin_object(w)
    }
    fn end_object<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.end_object(w)
    }
    fn begin_object_key<W: ?Sized + io::Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
        self.inner.begin_object_key(w, first)
    }
    fn end_object_key<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.end_object_key(w)
    }
    fn begin_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.begin_object_value(w)
    }
    fn end_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.inner.end_object_value(w)
    }
}

/// `smart_str_append_double(..., precision = 17 (serialize_precision -1), zero_frac)`.
pub fn php_json_float(v: f64, zero_frac: bool) -> String {
    if !v.is_finite() {
        // json_encode fails on INF/NAN (returns false); PARTIAL_OUTPUT would give 0.
        return "0".into();
    }
    let abs = v.abs();
    // zend_gcvt(precision 17): exponent form when decpt < -3 or decpt > 17.
    if v != 0.0 && !(1e-4..1e17).contains(&abs) {
        // zend_gcvt exponent form: mantissa always has a fractional part.
        let s = format!("{v:e}");
        let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let mant = if mant.contains('.') { mant.to_string() } else { format!("{mant}.0") };
        let exp: i32 = exp.parse().unwrap_or(0);
        return format!("{mant}e{}{}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    let s = format!("{v}");
    if zero_frac && !s.contains('.') { format!("{s}.0") } else { s }
}

/// `json_encode($value, $flags)`.
pub fn to_string_with_flags<T: Serialize + ?Sized>(value: &T, flags: u32) -> serde_json::Result<String> {
    let mut out = Vec::with_capacity(128);
    if flags & JSON_PRETTY_PRINT != 0 {
        let fmt = PhpFormatter { inner: PrettyFormatter::with_indent(b"    "), flags };
        value.serialize(&mut serde_json::Serializer::with_formatter(&mut out, fmt))?;
    } else {
        let fmt = PhpFormatter { inner: CompactFormatter, flags };
        value.serialize(&mut serde_json::Serializer::with_formatter(&mut out, fmt))?;
    }
    // The formatter only ever writes valid UTF-8.
    Ok(String::from_utf8(out).unwrap_or_default())
}

/// `json_encode($value)` with default flags.
pub fn to_string<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<String> {
    to_string_with_flags(value, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_escaping() {
        let v = json!({"url": "http://x/y", "u": "ü😀", "c": "\u{1}\n\t\"\\", "e": [], "o": {}});
        assert_eq!(
            to_string(&v).unwrap(),
            concat!(
                r#"{"url":"http:\/\/x\/y","u":""#,
                "\\u00fc\\ud83d\\ude00",
                r#"","c":"\u0001\n\t\"\\","e":[],"o":{}}"#
            )
        );
        assert_eq!(
            to_string_with_flags(&v, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE).unwrap(),
            "{\"url\":\"http://x/y\",\"u\":\"ü😀\",\"c\":\"\\u0001\\n\\t\\\"\\\\\",\"e\":[],\"o\":{}}"
        );
    }

    #[test]
    fn pretty_and_floats() {
        let v = json!({"a": [1, 2.5, 1.0], "b": {}});
        assert_eq!(
            to_string_with_flags(&v, JSON_PRETTY_PRINT).unwrap(),
            "{\n    \"a\": [\n        1,\n        2.5,\n        1\n    ],\n    \"b\": {}\n}"
        );
        assert_eq!(php_json_float(1.0, true), "1.0");
        assert_eq!(php_json_float(0.1, false), "0.1");
        // Reference output of PHP 8.5 json_encode().
        let cases: &[(f64, &str)] = &[
            (1.0, "1"),
            (0.1, "0.1"),
            (1e20, "1.0e+20"),
            (1e-5, "1.0e-5"),
            (0.0001, "0.0001"),
            (0.00012, "0.00012"),
            (1.5e15, "1500000000000000"),
            (1e15, "1000000000000000"),
            (999999999999999.0, "999999999999999"),
            (-0.0, "-0"),
            (3.0e-7, "3.0e-7"),
            (2.5, "2.5"),
            (1.0 / 3.0, "0.3333333333333333"),
            (1e14, "100000000000000"),
        ];
        for (v, want) in cases {
            assert_eq!(php_json_float(*v, false), *want, "{v}");
        }
    }
}
