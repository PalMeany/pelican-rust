//! Port of Symfony YAML 7.4 `Yaml::dump($value, 10, 2, DUMP_MULTI_LINE_LITERAL_BLOCK | DUMP_OBJECT_AS_MAP)`
//! (`vendor/symfony/yaml/{Dumper,Inline,Escaper}.php`).
//!
//! PHP arrays are represented as `serde_json::Value`: JSON arrays are PHP lists,
//! objects are PHP maps (keys that are canonical integers are PHP int keys).

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::Value;

/// `Yaml::dump($value, 10, 2, DUMP_MULTI_LINE_LITERAL_BLOCK | DUMP_OBJECT_AS_MAP)`.
pub fn dump(value: &Value) -> String {
    do_dump(value, 10, 0)
}

thread_local! {
    static EMPTY_AS_SEQUENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `Yaml::dump($value, $inline, 2, DUMP_EMPTY_ARRAY_AS_SEQUENCE)` (e.g. `Node::getYamlConfiguration()`):
/// empty arrays become `[]` instead of `{  }`.
pub fn dump_empty_as_sequence(value: &Value, inline: i32) -> String {
    EMPTY_AS_SEQUENCE.with(|f| f.set(true));
    let out = do_dump(value, inline, 0);
    EMPTY_AS_SEQUENCE.with(|f| f.set(false));
    out
}

fn empty_collection() -> String {
    if EMPTY_AS_SEQUENCE.with(|f| f.get()) { "[]".into() } else { "{  }".into() }
}

/// `Dumper::doDump()` (Dumper.php:52-145). Only the flags/blocks reachable with
/// `DUMP_MULTI_LINE_LITERAL_BLOCK | DUMP_OBJECT_AS_MAP` are implemented; the
/// compact-nested-mapping / tagged-value branches cannot trigger.
fn do_dump(value: &Value, inline: i32, indent: usize) -> String {
    let mut out = String::new();
    let prefix = " ".repeat(indent);

    let len = match value {
        Value::Array(a) => Some(a.len()),
        Value::Object(o) => Some(o.len()),
        _ => None,
    };
    // `if ($inline <= 0 || (!is_array($input) && !$input instanceof TaggedValue && $dumpObjectAsInlineMap) || !$input)`
    // — for scalars, empty containers and exhausted inline levels.
    if inline <= 0 || len.is_none() || len == Some(0) {
        out.push_str(&prefix);
        out.push_str(&inline_dump(value));
        return out;
    }

    let dump_as_map = is_hash_like(value);

    let emit = |out: &mut String, key: Option<&str>, val: &Value| {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }

        // Literal block (Dumper.php:79-101): strings containing "\n" but no "\r",
        // checked before the inline decision.
        if let Value::String(s) = val {
            if s.contains('\n') && !s.contains('\r') {
                out.push_str(&prefix);
                if let Some(key) = key.filter(|_| dump_as_map) {
                    out.push_str(&inline_dump_key(key));
                    out.push(':');
                } else {
                    out.push('-');
                }
                out.push_str(" |");
                out.push_str(block_indentation_indicator(s));
                out.push_str(block_chomping_indicator(s));
                for row in s.split('\n') {
                    if row.is_empty() {
                        out.push('\n');
                    } else {
                        out.push_str(&format!("\n{prefix}  {row}"));
                    }
                }
                return;
            }
        }

        let will_be_inlined =
            inline - 1 <= 0 || !matches!(val, Value::Array(_) | Value::Object(_)) || is_empty_collection(val);

        out.push_str(&prefix);
        if let Some(key) = key.filter(|_| dump_as_map) {
            out.push_str(&inline_dump_key(key));
            out.push(':');
            out.push(if will_be_inlined { ' ' } else { '\n' });
        } else {
            out.push('-');
            out.push(if will_be_inlined { ' ' } else { '\n' });
        }
        if will_be_inlined {
            // `doDump($value, $inline - 1, 0, ...)`: indent 0 means a bare inline dump.
            out.push_str(&do_dump(val, inline - 1, 0));
            out.push('\n');
        } else {
            out.push_str(&do_dump(val, inline - 1, indent + 2));
        }
    };

    match value {
        Value::Array(items) => {
            for val in items {
                emit(&mut out, None, val);
            }
        }
        Value::Object(map) => {
            for (key, val) in map {
                emit(&mut out, Some(key), val);
            }
        }
        _ => unreachable!("checked above"),
    }

    out
}

/// `getBlockIndentationIndicator()` (Dumper.php:169-183): "2" when the first
/// non-blank line starts with a space.
fn block_indentation_indicator(value: &str) -> &'static str {
    for line in value.split('\n') {
        if !line.trim_matches(' ').is_empty() {
            return if line.starts_with(' ') { "2" } else { "" };
        }
    }
    ""
}

/// Chomping indicator (Dumper.php:82-88): "+" when ending with "\n\n", "" (clip)
/// with a single trailing "\n", "-" otherwise.
fn block_chomping_indicator(value: &str) -> &'static str {
    let b = value.as_bytes();
    if b.len() >= 2 && b[b.len() - 2] == b'\n' && b[b.len() - 1] == b'\n' {
        "+"
    } else if b.last() == Some(&b'\n') {
        ""
    } else {
        "-"
    }
}

/// Empty `array`/`stdClass` check (`!$input`).
fn is_empty_collection(value: &Value) -> bool {
    match value {
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

/// `Inline::isHash()` (Inline.php:214-229): a PHP array is a list iff its keys are
/// exactly 0, 1, 2, … A JSON object whose keys are the canonical decimal forms of a
/// 0-based sequence maps to such a list.
fn is_hash_like(value: &Value) -> bool {
    let Value::Object(map) = value else {
        return false; // JSON arrays are always PHP lists (sequential keys)
    };
    let mut expected: i64 = 0;
    for key in map.keys() {
        match key.parse::<i64>() {
            Ok(n) if n == expected && n.to_string() == key.as_str() => {}
            _ => return true,
        }
        expected += 1;
    }
    false
}

/// Whether a JSON object represents a PHP list (used for inline `[...]` flow dumps).
fn object_is_list(map: &serde_json::Map<String, Value>) -> bool {
    !is_hash_like(&Value::Object(map.clone()))
}

/// `Inline::dump()` (Inline.php:115-209) for scalars and flow containers.
fn inline_dump(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Number(n) => dump_number(n),
        Value::String(s) => dump_scalar_string(s),
        Value::Array(items) => {
            if items.is_empty() {
                // Empty array: `dumpHashArray([])` → `sprintf('{ %s }', '')`.
                empty_collection()
            } else {
                // Non-empty JSON arrays are always PHP lists.
                let parts: Vec<String> = items.iter().map(inline_dump).collect();
                format!("[{}]", parts.join(", "))
            }
        }
        Value::Object(map) => {
            if map.is_empty() {
                empty_collection()
            } else if object_is_list(map) {
                // Sequential-int-keyed hash dumps as a flow sequence.
                let parts: Vec<String> = map.values().map(inline_dump).collect();
                format!("[{}]", parts.join(", "))
            } else {
                let parts: Vec<String> =
                    map.iter().map(|(k, v)| format!("{}: {}", inline_dump_key(k), inline_dump(v))).collect();
                format!("{{ {} }}", parts.join(", "))
            }
        }
    }
}

/// Map keys are dumped with the same scalar rules (`Inline::dump($key, $keyFlags)`).
fn inline_dump_key(key: &str) -> String {
    // Canonical integer string keys are PHP int keys (`Inline::dump` of an int is bare).
    match key.parse::<i64>() {
        Ok(n) if n.to_string() == key => n.to_string(),
        _ => dump_scalar_string(key),
    }
}

fn dump_number(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    match n.as_f64() {
        Some(f) => dump_float(f),
        None => "null".into(), // unreachable for serde_json numbers
    }
}

/// Float branch of `Inline::dump()` (Inline.php:160-184): `(string)$value` with
/// Symfony's whole-number `.0` suffix.
fn dump_float(v: f64) -> String {
    if v.is_nan() {
        return ".NaN".into();
    }
    if v.is_infinite() {
        // `(string)` gives "INF"/"-INF", then `str_ireplace('INF', '.Inf', ...)`.
        return if v > 0.0 { ".Inf".into() } else { "-.Inf".into() };
    }
    let repr = if v == 0.0 { if v.is_sign_negative() { "-0" } else { "0" }.to_string() } else { php_gcvt(v) };
    // `floor($value) == $value && $repr == $value` (numeric loose comparison),
    // then append ".0" unless the repr already carries an exponent.
    if v.floor() == v && repr.parse::<f64>() == Ok(v) && !repr.contains('E') {
        return format!("{repr}.0");
    }
    repr
}

/// PHP `(string)$float` ≙ `php_gcvt($value, precision=14, '.', 'E')`
/// (`main/snprintf.c`): fixed notation unless the decimal exponent is < -4 or the
/// value is ≥ 1e14; the scientific mantissa keeps at least one fractional digit.
fn php_gcvt(v: f64) -> String {
    let neg = v < 0.0;
    // 14 significant digits, correctly rounded.
    let s = format!("{:.13e}", v.abs());
    let Some((mant, exp_str)) = s.split_once('e') else {
        return s;
    };
    let exp: i32 = exp_str.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    // Decimal-point position: digits are d1.d2d3… × 10^exp.
    let decpt = exp + 1;

    if decpt < -3 || decpt > 14 {
        let m = if digits.len() == 1 { format!("{digits}.0") } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        return format!("{}{}E{}{}", if neg { "-" } else { "" }, m, if exp < 0 { '-' } else { '+' }, exp.abs());
    }

    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if decpt <= 0 {
        out.push_str("0.");
        for _ in 0..(-decpt) {
            out.push('0');
        }
        out.push_str(digits);
    } else if decpt as usize >= digits.len() {
        out.push_str(digits);
        for _ in 0..(decpt as usize - digits.len()) {
            out.push('0');
        }
    } else {
        out.push_str(&digits[..decpt as usize]);
        out.push('.');
        out.push_str(&digits[decpt as usize..]);
    }
    out
}

/// Scalar branch of `Inline::dump()` for strings (Inline.php:185-208).
fn dump_scalar_string(s: &str) -> String {
    // `is_numeric($value) && false === strpbrk($value, "\f\n\r\t\v")` → "'$value'"
    if is_numeric_php(s) {
        return format!("'{s}'");
    }
    // `'' == $value`
    if s.is_empty() {
        return "''".into();
    }
    // `isBinaryString()` (Inline.php:950-953): the regex only rejects ASCII bytes
    // outside {\x00, \x07-\x0d, \x1b, \x20-\xff}; multibyte UTF-8 bytes pass.
    if s.bytes().any(|b| matches!(b, 0x01..=0x06 | 0x0e..=0x1a | 0x1c..=0x1f)) {
        return format!("!!binary {}", BASE64_STANDARD.encode(s.as_bytes()));
    }
    if requires_double_quoting(s) {
        return escape_with_double_quotes(s);
    }
    if requires_single_quoting(s) {
        let single = escape_with_single_quotes(s);
        if !s.contains('\'') {
            return single;
        }
        // Attempt double-quoting to see if it's more efficient (byte-length tie → single).
        let double = escape_with_double_quotes(s);
        return if double.len() < single.len() { double } else { single };
    }
    if is_digits_underscores(s) || is_octal_notation(s) || is_hex_notation(s) || is_yaml_timestamp(s) {
        return escape_with_single_quotes(s);
    }
    s.to_string()
}

/// PHP `is_numeric()` (optional sign, digits/frac/exp; leading and trailing
/// " \t\n\r\v\f" allowed).
fn is_numeric_php(s: &str) -> bool {
    let t = s.trim_matches([' ', '\t', '\n', '\r', '\u{b}', '\u{c}']);
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let mut digits = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    let mut frac = 0;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            frac += 1;
        }
    }
    if digits + frac == 0 {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let mut exp = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            exp += 1;
        }
        if exp == 0 {
            return false;
        }
    }
    i == b.len()
}

fn requires_double_quoting(s: &str) -> bool {
    // REGEX_CHARACTER_TO_ESCAPE: [\x00-\x1f] | \x7f | U+0085 | U+00A0 | U+2028 | U+2029
    s.chars().any(|c| matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}' | '\u{85}' | '\u{a0}' | '\u{2028}' | '\u{2029}'))
}

const RESERVED_WORDS: [&str; 10] = ["null", "~", "true", "false", "y", "n", "yes", "no", "on", "off"];

fn requires_single_quoting(s: &str) -> bool {
    if RESERVED_WORDS.iter().any(|w| s.eq_ignore_ascii_case(w)) {
        return true;
    }
    // `/[\s\'\"\:\{\}\[\],&\*\#\?] | \A[\-?|<>=!%@`\p{Zs}]/xu`
    if s.chars().any(|c| {
        matches!(
            c,
            ' ' | '\t'
                | '\n'
                | '\u{b}'
                | '\u{c}'
                | '\r'
                | '\''
                | '"'
                | ':'
                | '{'
                | '}'
                | '['
                | ']'
                | ','
                | '&'
                | '*'
                | '#'
                | '?'
        )
    }) {
        return true;
    }
    s.chars().next().is_some_and(|c| {
        matches!(c, '-' | '?' | '|' | '<' | '>' | '=' | '!' | '%' | '@' | '`') || is_unicode_space_separator(c)
    })
}

/// Unicode category Zs (PCRE `\p{Zs}`).
fn is_unicode_space_separator(c: char) -> bool {
    matches!(
        c,
        '\u{20}' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

/// `Escaper::escapeWithSingleQuotes()`.
fn escape_with_single_quotes(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// `Escaper::escapeWithDoubleQuotes()`: `str_replace(ESCAPEES, ESCAPED, $value)`
/// applies replacements sequentially, which converges to proper YAML escaping
/// (backslash → `\\`, quote → `\"`). Mirror that iteration order exactly.
fn escape_with_double_quotes(s: &str) -> String {
    let mut t = String::with_capacity(s.len());
    t.push_str(s);
    for (from, to) in ESCAPE_TABLE {
        t = t.replace(from, to);
    }
    format!("\"{t}\"")
}

const BS: &str = "\\";
static ESCAPE_TABLE: &[(&str, &str)] = &[
    // '\\' → '\\\\', '\\\\' → '\\"', '\\"' → '\\\\', '"' → '\\"'
    (BS, "\\\\"),
    ("\\\\", "\\\""),
    ("\\\"", "\\\\"),
    ("\"", "\\\""),
    ("\u{0}", "\\0"),
    ("\u{1}", "\\x01"),
    ("\u{2}", "\\x02"),
    ("\u{3}", "\\x03"),
    ("\u{4}", "\\x04"),
    ("\u{5}", "\\x05"),
    ("\u{6}", "\\x06"),
    ("\u{7}", "\\a"),
    ("\u{8}", "\\b"),
    ("\t", "\\t"),
    ("\n", "\\n"),
    ("\u{b}", "\\v"),
    ("\u{c}", "\\f"),
    ("\r", "\\r"),
    ("\u{e}", "\\x0e"),
    ("\u{f}", "\\x0f"),
    ("\u{10}", "\\x10"),
    ("\u{11}", "\\x11"),
    ("\u{12}", "\\x12"),
    ("\u{13}", "\\x13"),
    ("\u{14}", "\\x14"),
    ("\u{15}", "\\x15"),
    ("\u{16}", "\\x16"),
    ("\u{17}", "\\x17"),
    ("\u{18}", "\\x18"),
    ("\u{19}", "\\x19"),
    ("\u{1a}", "\\x1a"),
    ("\u{1b}", "\\e"),
    ("\u{1c}", "\\x1c"),
    ("\u{1d}", "\\x1d"),
    ("\u{1e}", "\\x1e"),
    ("\u{1f}", "\\x1f"),
    ("\u{7f}", "\\x7f"),
    // \xc2\x85, \xc2\xa0, \xe2\x80\xa8, \xe2\x80\xa9
    ("\u{85}", "\\N"),
    ("\u{a0}", "\\_"),
    ("\u{2028}", "\\L"),
    ("\u{2029}", "\\P"),
];

// --- post-single-quoting numeric/date patterns ---

/// `{^[0-9]+[_0-9]*$}`
fn is_digits_underscores(s: &str) -> bool {
    let b = s.as_bytes();
    let head = b.iter().position(|c| !c.is_ascii_digit()).unwrap_or(b.len());
    head > 0 && b[head..].iter().all(|c| *c == b'_' || c.is_ascii_digit())
}

/// `{^[+-]?0o[0-7_]++$}`
fn is_octal_notation(s: &str) -> bool {
    let b = s.strip_prefix(['+', '-']).unwrap_or(s).as_bytes();
    match b.strip_prefix(b"0o") {
        Some(rest) => !rest.is_empty() && rest.iter().all(|c| (*c == b'_') || (b'0'..=b'7').contains(c)),
        None => false,
    }
}

/// `~^0x[0-9a-fA-F_]++$~`
fn is_hex_notation(s: &str) -> bool {
    match s.strip_prefix("0x") {
        Some(rest) => !rest.is_empty() && rest.chars().all(|c| c.is_ascii_hexdigit() || c == '_'),
        None => false,
    }
}

/// YAML timestamp regex (Inline.php::getTimestampRegex); without the `/m` or `/D`
/// modifiers `$` also accepts a single trailing newline.
fn is_yaml_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    let n = b.len();
    let mut i = 0;

    fn digits(b: &[u8], i: &mut usize, min: usize, max: usize) -> bool {
        let mut count = 0;
        while *i < b.len() && b[*i].is_ascii_digit() && count < max {
            *i += 1;
            count += 1;
        }
        count >= min
    }

    // (?P<year>[0-9]{4})-(?P<month>\d\d?)-(?P<day>\d\d?)
    if !(digits(b, &mut i, 4, 4)
        && i < n
        && b[i] == b'-'
        && {
            i += 1;
            digits(b, &mut i, 1, 2)
        }
        && i < n
        && b[i] == b'-'
        && {
            i += 1;
            digits(b, &mut i, 1, 2)
        })
    {
        return false;
    }
    let after_date = i;

    // Optional time part: (?:([Tt]|[ \t]+) h:m:s(.fraction)?(tz)?
    if i < n && (b[i] == b'T' || b[i] == b't' || b[i] == b' ' || b[i] == b'\t') {
        if b[i] == b'T' || b[i] == b't' {
            i += 1;
        } else {
            while i < n && (b[i] == b' ' || b[i] == b'\t') {
                i += 1;
            }
        }
        let ok = digits(b, &mut i, 1, 2)
            && i < n
            && b[i] == b':'
            && {
                i += 1;
                digits(b, &mut i, 2, 2)
            }
            && i < n
            && b[i] == b':'
            && {
                i += 1;
                digits(b, &mut i, 2, 2)
            };
        if ok {
            // (?:\.fraction)?
            if i < n && b[i] == b'.' {
                i += 1;
                digits(b, &mut i, 0, usize::MAX);
            }
            // (?:[ \t]*(?:Z|[+-]\d{1,2}(?::\d{2})?))?
            let mut j = i;
            while j < n && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            if j < n
                && (b[j] == b'Z'
                    || ((b[j] == b'+' || b[j] == b'-') && {
                        j += 1;
                        digits(b, &mut j, 1, 2)
                            && (j >= n
                                || (b[j] == b':' && {
                                    j += 1;
                                    digits(b, &mut j, 2, 2)
                                }))
                    }))
            {
                i = j;
            }
        } else {
            // Time group failed → regex backtracks to no-time; require end right here.
            return check_end(b, after_date, n);
        }
    }

    check_end(b, i, n)
}

fn check_end(b: &[u8], i: usize, n: usize) -> bool {
    i == n || (i == n - 1 && b[i] == b'\n')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn float_repr() {
        assert_eq!(dump_float(0.0), "0.0");
        assert_eq!(dump_float(1.0), "1.0");
        assert_eq!(dump_float(-0.0), "-0.0");
        assert_eq!(dump_float(0.1), "0.1");
        assert_eq!(dump_float(1.5), "1.5");
        assert_eq!(dump_float(-2.25), "-2.25");
        assert_eq!(dump_float(123456789.0), "123456789.0");
        assert_eq!(dump_float(1.0e20), "1.0E+20");
        assert_eq!(dump_float(1.0e-7), "1.0E-7");
        assert_eq!(dump_float(1.0 / 3.0), "0.33333333333333");
        assert_eq!(dump_float(1.0e14), "1.0E+14");
        assert_eq!(dump_float(1.0e13), "10000000000000.0");
        assert_eq!(dump_float(1.0e-4), "0.0001");
        assert_eq!(dump_float(1.0e-5), "1.0E-5");
    }

    #[test]
    fn scalar_strings() {
        assert_eq!(dump_scalar_string(""), "''");
        assert_eq!(dump_scalar_string("25565"), "'25565'");
        assert_eq!(dump_scalar_string("back\\slash"), "back\\slash");
        assert_eq!(dump_scalar_string("x'y"), "\"x'y\"");
        assert_eq!(dump_scalar_string("x'\"y"), "'x''\"y'");
        assert_eq!(dump_scalar_string("1e"), "1e");
        assert_eq!(dump_scalar_string("---"), "'---'");
        assert_eq!(dump_scalar_string("..."), "...");
        assert_eq!(dump_scalar_string("2026-06-01"), "'2026-06-01'");
        assert_eq!(dump_scalar_string("2026-06-01T01:02:03+00:00"), "'2026-06-01T01:02:03+00:00'");
        assert_eq!(dump_scalar_string("tab\there"), "\"tab\\there\"");
    }

    #[test]
    fn structure() {
        assert_eq!(dump(&json!([])), "{  }");
        assert_eq!(dump(&json!({})), "{  }");
        assert_eq!(dump(&json!({"a": []})), "a: {  }\n");
        assert_eq!(dump(&json!(["x", "y"])), "- x\n- 'y'\n");
        assert_eq!(dump(&json!({"0": "a", "2": "b"})), "0: a\n2: b\n");
        assert_eq!(dump(&json!({"vars": [{"name": "A"}]})), "vars:\n  -\n    name: A\n");
    }
}
