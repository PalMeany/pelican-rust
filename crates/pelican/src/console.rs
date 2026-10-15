//! Laravel console I/O for the `p:*` commands (Illuminate\Console\Command over
//! SymfonyStyle): questions, secrets, confirmations, choices, styled lines and tables.
//! Colors only on a terminal; `--no-interaction` answers every question with its default.

use std::io::{BufRead, IsTerminal, Write};

pub struct Console {
    interactive: bool,
    ansi: bool,
}

impl Console {
    pub fn new(no_interaction: bool) -> Console {
        Console {
            interactive: !no_interaction && std::io::stdin().is_terminal(),
            ansi: std::io::stdout().is_terminal(),
        }
    }

    /// `$this->input->isInteractive()`.
    pub fn interactive(&self) -> bool {
        self.interactive
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.ansi { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_string() }
    }

    /// `$this->line()`.
    pub fn line(&self, text: &str) {
        println!("{text}");
    }

    /// `$this->info()` (`<info>`: green).
    pub fn info(&self, text: &str) {
        println!("{}", self.paint("32", text));
    }

    /// `$this->comment()` / `$this->warn()` (`<comment>`: yellow).
    pub fn warn(&self, text: &str) {
        println!("{}", self.paint("33", text));
    }

    /// `$this->error()` (`<error>`: white on red).
    pub fn error(&self, text: &str) {
        eprintln!("{}", self.paint("37;41", text));
    }

    /// `$this->output->warning()`: SymfonyStyle's `[WARNING]` block.
    pub fn warning_block(&self, text: &str) {
        println!();
        println!("{}", self.paint("30;43", &format!(" [WARNING] {text} ")));
        println!();
    }

    /// `$this->output->success()`: SymfonyStyle's `[OK]` block.
    pub fn success_block(&self, text: &str) {
        println!();
        println!("{}", self.paint("30;42", &format!(" [OK] {text} ")));
        println!();
    }

    fn prompt(&self, question: &str, default: Option<&str>, suffix: &str) {
        let q = self.paint("32", question);
        match default {
            Some(d) => println!(" {q}{suffix} [{}]:", self.paint("33", d)),
            None => println!(" {q}{suffix}:"),
        }
        print!(" > ");
        let _ = std::io::stdout().flush();
    }

    fn read_line(&self) -> Option<String> {
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_string()),
        }
    }

    /// `$this->ask($question, $default)`: the default when not interactive or empty.
    pub fn ask(&self, question: &str, default: Option<&str>) -> Option<String> {
        if !self.interactive {
            return default.map(str::to_string);
        }
        self.prompt(question, default, "");
        let answer = self.read_line().unwrap_or_default();
        println!();
        if answer.trim().is_empty() { default.map(str::to_string) } else { Some(answer.trim().to_string()) }
    }

    /// `$this->secret($question)`: hidden input (`None` when not interactive).
    pub fn secret(&self, question: &str) -> Option<String> {
        if !self.interactive {
            return None;
        }
        self.prompt(question, None, "");
        let answer = rpassword::read_password().ok().filter(|s| !s.is_empty());
        println!();
        answer
    }

    /// `$this->confirm($question, $default)`.
    pub fn confirm(&self, question: &str, default: bool) -> bool {
        if !self.interactive {
            return default;
        }
        self.prompt(question, Some(if default { "yes" } else { "no" }), " (yes/no)");
        let answer = self.read_line().unwrap_or_default().trim().to_lowercase();
        println!();
        if answer.is_empty() { default } else { answer.starts_with('y') }
    }

    /// `$this->choice($question, $choices, $default)`: returns one of `choices` (by value or
    /// by its index).
    pub fn choice(&self, question: &str, choices: &[&str], default: Option<&str>) -> Option<String> {
        if !self.interactive {
            return default.map(str::to_string);
        }
        loop {
            self.prompt(question, default, "");
            for (i, c) in choices.iter().enumerate() {
                println!("  [{}] {c}", self.paint("33", &i.to_string()));
            }
            print!(" > ");
            let _ = std::io::stdout().flush();
            let answer = self.read_line().unwrap_or_default().trim().to_string();
            println!();
            if answer.is_empty() {
                if let Some(d) = default {
                    return Some(d.to_string());
                }
            }
            if let Some(c) = choices.iter().find(|c| **c == answer) {
                return Some(c.to_string());
            }
            if let Some(c) = answer.parse::<usize>().ok().and_then(|i| choices.get(i)) {
                return Some(c.to_string());
            }
            self.error(&format!("Value \"{answer}\" is invalid"));
        }
    }

    /// `$this->table($headers, $rows)`: Symfony's default table style.
    pub fn table(&self, headers: &[&str], rows: &[Vec<String>]) {
        print!("{}", render_table(headers, rows));
    }
}

/// Symfony `Table` (style `default`): `+---+` borders, one space of padding.
pub fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let cols = headers.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
    let width = |s: &str| s.chars().count();
    let mut widths = vec![0usize; cols];
    for (i, h) in headers.iter().enumerate() {
        widths[i] = widths[i].max(width(h));
    }
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(width(c));
        }
    }
    let border = format!("+{}+\n", widths.iter().map(|w| "-".repeat(w + 2)).collect::<Vec<_>>().join("+"));
    let row = |cells: Vec<&str>| {
        let parts: Vec<String> = (0..cols)
            .map(|i| {
                let c = cells.get(i).copied().unwrap_or("");
                format!(" {c}{} ", " ".repeat(widths[i] - width(c)))
            })
            .collect();
        format!("|{}|\n", parts.join("|"))
    };
    let mut out = border.clone();
    if !headers.is_empty() {
        out.push_str(&row(headers.to_vec()));
        out.push_str(&border);
    }
    for r in rows {
        out.push_str(&row(r.iter().map(String::as_str).collect()));
    }
    out.push_str(&border);
    out
}

/// PHP truthiness of an option value (`--admin=1`, `--public=0`).
pub fn truthy(v: &str) -> bool {
    !matches!(v, "" | "0")
}

/// `trans()` for console messages (the original's `lang/en/command/messages.php`).
pub fn t(key: &str, replace: &[(&str, &str)]) -> String {
    let params: Vec<(String, String)> = replace.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    pelican_web::i18n::trans("en", key, &params)
}

#[cfg(test)]
mod tests {
    #[test]
    fn symfony_table() {
        let t = super::render_table(
            &["Field", "Value"],
            &[vec!["UUID".into(), "abc".into()], vec!["Admin".into(), "Yes".into()]],
        );
        assert_eq!(
            t,
            "+-------+-------+\n| Field | Value |\n+-------+-------+\n| UUID  | abc   |\n| Admin | Yes   |\n+-------+-------+\n"
        );
    }
}
