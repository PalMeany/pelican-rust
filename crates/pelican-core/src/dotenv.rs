//! `.env` parsing as vlucas/phpdotenv 5 does it for Laravel (`Dotenv\Parser\*`, `Dotenv\Loader\*`).
//!
//! Differences from other dotenv dialects matter for real files: only `${NAME}` is expanded
//! (`"$2a$10$…"`, as in CurseForge API keys, stays literal), single quotes are literal, double
//! quotes take `\" \\ \$ \f \n \r \t \v` escapes and may span lines, unquoted values end at
//! whitespace, and a later line overrides an earlier one.

use std::collections::HashMap;

/// A parsed value: its characters and the byte offsets of `$` that may start `${NAME}`.
#[derive(Clone, Debug, PartialEq)]
pub struct Value {
    chars: String,
    vars: Vec<usize>,
}

/// `Dotenv\Parser\Parser::parse`: entries in file order; `None` for a name without `=`.
pub fn parse(content: &str) -> Result<Vec<(String, Option<Value>)>, String> {
    let lines = split_lines(content);
    let mut out = Vec::new();
    for raw in process_lines(&lines) {
        out.push(parse_entry(&raw).map_err(|e| format!("Failed to parse dotenv file. {e}"))?);
    }
    Ok(out)
}

/// `Dotenv\Loader\Loader::load` with Laravel's immutable repository: variables of the process
/// environment (`external`) are never replaced, the file's own may be redefined by later lines.
pub fn load(content: &str, external: &HashMap<String, String>) -> Result<HashMap<String, String>, String> {
    let mut vars = external.clone();
    for (name, value) in parse(content)? {
        if external.contains_key(&name) {
            continue;
        }
        match value {
            Some(v) => {
                let resolved = resolve(&v, &vars);
                vars.insert(name, resolved);
            }
            None => {
                vars.remove(&name);
            }
        }
    }
    Ok(vars)
}

/// `Dotenv\Loader\Resolver::resolve`: replace `${NAME}` at the recorded `$` offsets, last first.
fn resolve(value: &Value, vars: &HashMap<String, String>) -> String {
    if value.vars.is_empty() {
        return value.chars.clone();
    }
    let mut offsets = value.vars.clone();
    offsets.sort_unstable_by(|a, b| b.cmp(a));
    let mut tail = String::new();
    let mut cut = value.chars.len();
    for i in offsets {
        let segment = format!("{}{tail}", &value.chars[i..cut]);
        tail = resolve_variable(&segment, vars);
        cut = i;
    }
    format!("{}{tail}", &value.chars[..cut])
}

/// `/\A\${([a-zA-Z0-9_.]+)}/` replaced by the variable's value when it is defined.
fn resolve_variable(s: &str, vars: &HashMap<String, String>) -> String {
    let Some(rest) = s.strip_prefix("${") else {
        return s.to_string();
    };
    let Some(end) = rest.find('}') else {
        return s.to_string();
    };
    let name = &rest[..end];
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.') {
        return s.to_string();
    }
    match vars.get(name) {
        Some(v) => format!("{v}{}", &rest[end + 1..]),
        None => s.to_string(),
    }
}

/// `Regex::split("/(\r\n|\n|\r)/", $content)`.
fn split_lines(content: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let bytes = content.as_bytes();
    let (mut start, mut i) = (0, 0);
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                lines.push(&content[start..i]);
                i += if bytes.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
                start = i;
            }
            b'\n' => {
                lines.push(&content[start..i]);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    lines.push(&content[start..]);
    lines
}

/// `Dotenv\Parser\Lines::process`: join multi-line double-quoted values, drop comments and blanks.
fn process_lines(lines: &[&str]) -> Vec<String> {
    let mut output = Vec::new();
    let mut multiline = false;
    let mut buffer: Vec<&str> = Vec::new();
    for &line in lines {
        let starts_here = !multiline && looks_like_multiline_start(line);
        if starts_here {
            multiline = true;
        }
        let mut current = line.to_string();
        if multiline {
            buffer.push(line);
            if looks_like_multiline_stop(line, starts_here) {
                multiline = false;
                current = buffer.join("\n");
                buffer.clear();
            }
        }
        if !multiline && !is_comment_or_whitespace(&current) {
            output.push(current);
        }
    }
    output
}

fn looks_like_multiline_start(line: &str) -> bool {
    let Some(pos) = line.find("=\"") else {
        return false;
    };
    if line.find('#').is_some_and(|hash| hash < pos) {
        return false;
    }
    !looks_like_multiline_stop(line, true)
}

/// Counts `"` preceded by anything but a backslash, after removing `\\` pairs.
fn looks_like_multiline_stop(line: &str, started: bool) -> bool {
    if line == "\"" {
        return true;
    }
    let s = line.replace("\\\\", "");
    let b = s.as_bytes();
    let count = (1..b.len()).filter(|&i| b[i] == b'"' && b[i - 1] != b'\\').count();
    if started { count > 1 } else { count >= 1 }
}

/// PHP `trim()` default characters.
const TRIM: &[char] = &[' ', '\n', '\r', '\t', '\0', '\x0B'];

fn is_comment_or_whitespace(line: &str) -> bool {
    let t = line.trim_matches(TRIM);
    t.is_empty() || t.starts_with('#')
}

/// `Dotenv\Parser\EntryParser::parse`.
fn parse_entry(raw: &str) -> Result<(String, Option<Value>), String> {
    let (name, value) = match raw.split_once('=') {
        Some((n, v)) => (n.trim_matches(TRIM), Some(v.trim_matches(TRIM))),
        None => (raw, None),
    };
    if name.is_empty() {
        return Err(error("an unexpected equals", raw));
    }
    let name = parse_name(name)?;
    let value = match value {
        None => None,
        Some(v) => Some(parse_value(v)?),
    };
    Ok((name, value))
}

fn parse_name(name: &str) -> Result<String, String> {
    let mut name = name;
    if name.len() > 6 && name.starts_with("export") && name.as_bytes()[6].is_ascii_whitespace() {
        name = name[6..].trim_start_matches([' ', '\t', '\n', '\r', '\x0B', '\x0C']);
    }
    if name.len() >= 3
        && ((name.starts_with('"') && name.ends_with('"')) || (name.starts_with('\'') && name.ends_with('\'')))
    {
        name = &name[1..name.len() - 1];
    }
    let valid = !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.');
    if !valid {
        return Err(error("an invalid name", name));
    }
    Ok(name.to_string())
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Initial,
    Unquoted,
    SingleQuoted,
    DoubleQuoted,
    Escape,
    Whitespace,
    Comment,
}

fn parse_value(value: &str) -> Result<Value, String> {
    if value.trim_matches(TRIM).is_empty() {
        return Ok(Value { chars: String::new(), vars: Vec::new() });
    }
    if let Some(literal) = parse_literal(value) {
        return Ok(Value { chars: literal.to_string(), vars: Vec::new() });
    }
    let (mut chars, mut pending) = (String::new(), String::new());
    let mut vars = Vec::new();
    let mut state = State::Initial;
    for token in lex(value) {
        let (chunk, var, next) = process_token(state, token).map_err(|cause| error(cause, value))?;
        if var {
            chars.push_str(&pending);
            pending.clear();
            vars.push(chars.len());
        }
        pending.push_str(&chunk);
        state = next;
    }
    if matches!(state, State::SingleQuoted | State::DoubleQuoted | State::Escape) {
        return Err(error("a missing closing quote", value));
    }
    chars.push_str(&pending);
    Ok(Value { chars, vars })
}

/// `~\A(?|([^\s\\'"#$]{1,1000})|'([^']*)'|"([^"\\$]*)")\z~`.
fn parse_literal(value: &str) -> Option<&str> {
    let plain = |c: char| !(c.is_whitespace() || matches!(c, '\\' | '\'' | '"' | '#' | '$'));
    if value.chars().count() <= 1000 && value.chars().all(plain) {
        return Some(value);
    }
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        let inner = &value[1..value.len() - 1];
        if !inner.contains('\'') {
            return Some(inner);
        }
    }
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        let inner = &value[1..value.len() - 1];
        if !inner.contains(['"', '\\', '$']) {
            return Some(inner);
        }
    }
    None
}

/// `Dotenv\Parser\Lexer::lex`: newline runs, other whitespace runs, `\`, `'`, `"`, `#`, `$`,
/// and runs of anything else.
fn lex(content: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = content;
    while let Some(c) = rest.chars().next() {
        let len = match c {
            '\r' | '\n' => run_len(rest, |c| c == '\r' || c == '\n'),
            '\\' | '\'' | '"' | '#' | '$' => 1,
            c if c.is_whitespace() => run_len(rest, |c| c.is_whitespace() && c != '\r' && c != '\n'),
            _ => run_len(rest, |c| !(c.is_whitespace() || matches!(c, '\\' | '\'' | '"' | '#' | '$'))),
        };
        tokens.push(&rest[..len]);
        rest = &rest[len..];
    }
    tokens
}

/// Byte length of the leading run (at most 1000 characters) matching `pred`.
fn run_len(s: &str, pred: impl Fn(char) -> bool) -> usize {
    s.char_indices()
        .take(1000)
        .find(|&(_, c)| !pred(c))
        .map_or_else(|| s.char_indices().nth(1000).map_or(s.len(), |(i, _)| i), |(i, _)| i)
}

/// PHP `ctype_space()` on a token (whitespace tokens are runs of one class).
fn is_space(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0B' | b'\x0C'))
}

/// `EntryParser::processToken`: (chunk, starts a variable, next state).
fn process_token(state: State, token: &str) -> Result<(String, bool, State), &'static str> {
    let t = |chunk: &str, var: bool, next: State| Ok((chunk.to_string(), var, next));
    match state {
        State::Initial => match token {
            "'" => t("", false, State::SingleQuoted),
            "\"" => t("", false, State::DoubleQuoted),
            "#" => t("", false, State::Comment),
            "$" => t(token, true, State::Unquoted),
            _ => t(token, false, State::Unquoted),
        },
        State::Unquoted => match token {
            "#" => t("", false, State::Comment),
            "$" => t(token, true, State::Unquoted),
            _ if is_space(token) => t("", false, State::Whitespace),
            _ => t(token, false, State::Unquoted),
        },
        State::SingleQuoted => match token {
            "'" => t("", false, State::Whitespace),
            _ => t(token, false, State::SingleQuoted),
        },
        State::DoubleQuoted => match token {
            "\"" => t("", false, State::Whitespace),
            "\\" => t("", false, State::Escape),
            "$" => t(token, true, State::DoubleQuoted),
            _ => t(token, false, State::DoubleQuoted),
        },
        State::Escape => match token {
            "\"" | "\\" | "$" => t(token, false, State::DoubleQuoted),
            _ => {
                let mut chars = token.chars();
                let escaped = match chars.next() {
                    Some('f') => '\x0C',
                    Some('n') => '\n',
                    Some('r') => '\r',
                    Some('t') => '\t',
                    Some('v') => '\x0B',
                    _ => return Err("an unexpected escape sequence"),
                };
                Ok((format!("{escaped}{}", chars.as_str()), false, State::DoubleQuoted))
            }
        },
        State::Whitespace => match token {
            "#" => t("", false, State::Comment),
            _ if is_space(token) => t("", false, State::Whitespace),
            _ => Err("unexpected whitespace"),
        },
        State::Comment => t("", false, State::Comment),
    }
}

/// `EntryParser::getErrorMessage`: first line of the subject, cut at 80 bytes.
fn error(cause: &str, subject: &str) -> String {
    let mut line = subject.split('\n').next().unwrap_or("").to_string();
    if line.len() > 80 {
        let mut end = 80;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line = format!("{}...", &line[..end]);
    }
    let line: String = line
        .chars()
        .map(|c| if c.is_ascii_control() { format!("\\{:03o}", c as u32) } else { c.to_string() })
        .collect();
    format!("Encountered {cause} at [{line}].")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(content: &str) -> HashMap<String, String> {
        load(content, &HashMap::new()).expect("parses")
    }

    #[test]
    fn values_like_phpdotenv() {
        let v = env(concat!(
            "# comment\n",
            "A=plain\n",
            "B=\"$2a$10$abc/def.ghi\"\n",
            "C='single $X ${A}'\n",
            "D=\"double ${A} \\\"q\\\" \\$A \\n end\"\n",
            "E=unquoted # trailing comment\n",
            "export F=exported\n",
            "G=\n",
            "H=\"\"\n",
            "I=${A}/x\n",
            "J=\"${MISSING}\"\n",
            "K=\"line1\nline2\"\n",
            "A=redefined\n",
            "L=(parens)\n",
            "M=\"a#b\"\n",
        ));
        assert_eq!(v["A"], "redefined");
        assert_eq!(v["B"], "$2a$10$abc/def.ghi");
        assert_eq!(v["C"], "single $X ${A}");
        assert_eq!(v["D"], "double plain \"q\" $A \n end");
        assert_eq!(v["E"], "unquoted");
        assert_eq!(v["F"], "exported");
        assert_eq!(v["G"], "");
        assert_eq!(v["H"], "");
        assert_eq!(v["I"], "plain/x");
        assert_eq!(v["J"], "${MISSING}");
        assert_eq!(v["K"], "line1\nline2");
        assert_eq!(v["L"], "(parens)");
        assert_eq!(v["M"], "a#b");
    }

    /// An unterminated double-quoted value swallows the rest of the file without an error.
    #[test]
    fn unterminated_multiline_drops_the_rest() {
        let v = env("A=1\nB=\"open\nC=2\n");
        assert_eq!(v.get("A").map(String::as_str), Some("1"));
        assert!(!v.contains_key("B") && !v.contains_key("C"));
    }

    #[test]
    fn process_environment_wins() {
        let external = HashMap::from([("A".to_string(), "env".to_string())]);
        let v = load("A=file\nB=${A}\n", &external).expect("parses");
        assert_eq!(v["A"], "env");
        assert_eq!(v["B"], "env");
    }

    #[test]
    fn errors_like_phpdotenv() {
        let e = |s: &str| load(s, &HashMap::new()).expect_err("rejects");
        assert_eq!(
            e("A=two words\n"),
            "Failed to parse dotenv file. Encountered unexpected whitespace at [two words]."
        );
        assert_eq!(e("A='open\n"), "Failed to parse dotenv file. Encountered a missing closing quote at ['open].");
        assert_eq!(
            e("A=\"bad \\q\"\n"),
            "Failed to parse dotenv file. Encountered an unexpected escape sequence at [\"bad \\q\"]."
        );
        assert_eq!(e("=x\n"), "Failed to parse dotenv file. Encountered an unexpected equals at [=x].");
    }
}
