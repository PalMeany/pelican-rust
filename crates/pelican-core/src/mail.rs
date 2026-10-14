//! Mail notifications (`MailMessage` rendered with Laravel's default markdown
//! notification layout) over the configured mailer: `smtp`, `sendmail` or `log`
//! (`MAIL_*` env, config/mail.php). Anything else (`array`, unknown) drops mail.

use lettre::message::{Mailbox, MultiPart, SinglePart, header::ContentType};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSendmailTransport, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::env::Env;

#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("invalid address: {0}")]
    Address(String),
    #[error("transport: {0}")]
    Transport(String),
}

#[derive(Clone, Debug)]
pub struct MailConfig {
    pub mailer: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    /// `MAIL_SCHEME` / legacy `MAIL_ENCRYPTION`: `smtps`/`ssl`/`tls` = implicit TLS, else STARTTLS when offered.
    pub scheme: String,
    pub from_address: String,
    pub from_name: String,
    pub app_name: String,
}

impl MailConfig {
    pub fn from_env(env: &Env, app_name: &str) -> MailConfig {
        let opt = |k: &str| Some(env.string_or(k, "")).filter(|s| !s.is_empty());
        MailConfig {
            mailer: env.string_or("MAIL_MAILER", "log"),
            host: env.string_or("MAIL_HOST", "127.0.0.1"),
            port: env.int_or("MAIL_PORT", 2525).clamp(1, 65535) as u16,
            username: opt("MAIL_USERNAME"),
            password: opt("MAIL_PASSWORD"),
            scheme: opt("MAIL_SCHEME").or_else(|| opt("MAIL_ENCRYPTION")).unwrap_or_default().to_lowercase(),
            from_address: env.string_or("MAIL_FROM_ADDRESS", "hello@example.com"),
            from_name: env.string_or("MAIL_FROM_NAME", app_name),
            app_name: app_name.to_string(),
        }
    }
}

/// `Illuminate\Notifications\Messages\MailMessage` subset used by the panel.
#[derive(Clone, Debug, Default)]
pub struct MailMessage {
    pub subject: String,
    pub error: bool,
    pub greeting: Option<String>,
    pub lines: Vec<String>,
    pub action: Option<(String, String)>,
    /// Lines after the action button (`outroLines`).
    pub outro: Vec<String>,
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

impl MailMessage {
    /// Plain-text alternative (the notification markdown template, unrendered).
    pub fn text(&self, app_name: &str) -> String {
        let mut out = String::new();
        let greeting =
            self.greeting.clone().unwrap_or_else(|| if self.error { "Whoops!".into() } else { "Hello!".into() });
        out.push_str(&format!("# {greeting}\n\n"));
        for l in &self.lines {
            out.push_str(&format!("{l}\n\n"));
        }
        if let Some((text, url)) = &self.action {
            out.push_str(&format!("{text}: {url}\n\n"));
        }
        for l in &self.outro {
            out.push_str(&format!("{l}\n\n"));
        }
        out.push_str(&format!("Regards,\n{app_name}\n"));
        if let Some((text, url)) = &self.action {
            out.push_str(&format!(
                "\nIf you're having trouble clicking the \"{text}\" button, copy and paste the URL below\ninto your web browser: {url}\n"
            ));
        }
        out
    }

    /// HTML body in the spirit of Laravel's default mail layout.
    pub fn html(&self, app_name: &str) -> String {
        let greeting =
            self.greeting.clone().unwrap_or_else(|| if self.error { "Whoops!".into() } else { "Hello!".into() });
        let mut body = format!("<h1>{}</h1>", html_escape(&greeting));
        for l in &self.lines {
            body.push_str(&format!("<p>{}</p>", html_escape(l)));
        }
        let color = if self.error { "#b91c1c" } else { "#2d3748" };
        if let Some((text, url)) = &self.action {
            body.push_str(&format!(
                "<p style=\"text-align:center\"><a href=\"{}\" style=\"background:{color};color:#fff;padding:8px 18px;border-radius:4px;text-decoration:none\">{}</a></p>",
                html_escape(url),
                html_escape(text)
            ));
        }
        for l in &self.outro {
            body.push_str(&format!("<p>{}</p>", html_escape(l)));
        }
        body.push_str(&format!("<p>Regards,<br>{}</p>", html_escape(app_name)));
        if let Some((text, url)) = &self.action {
            body.push_str(&format!(
                "<hr><p style=\"font-size:12px\">If you're having trouble clicking the \"{}\" button, copy and paste the URL below into your web browser: <a href=\"{u}\">{u}</a></p>",
                html_escape(text),
                u = html_escape(url)
            ));
        }
        format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>{}</title></head><body style=\"font-family:sans-serif;color:#2d3748\"><h2>{}</h2>{body}</body></html>",
            html_escape(&self.subject),
            html_escape(app_name)
        )
    }
}

/// Send one message to `to` (`"Name <addr>"` style mailbox from name + address).
pub async fn send(cfg: &MailConfig, to_name: &str, to_address: &str, msg: &MailMessage) -> Result<(), MailError> {
    if cfg.mailer == "log" {
        tracing::info!(to = to_address, subject = %msg.subject, body = %msg.text(&cfg.app_name), "mail (log mailer)");
        return Ok(());
    }
    if cfg.mailer != "smtp" && cfg.mailer != "sendmail" {
        return Ok(());
    }
    let from: Mailbox = format!("{} <{}>", cfg.from_name, cfg.from_address)
        .parse()
        .map_err(|e: lettre::address::AddressError| MailError::Address(e.to_string()))?;
    let to: Mailbox = format!("{to_name} <{to_address}>")
        .parse()
        .or_else(|_| to_address.parse())
        .map_err(|e: lettre::address::AddressError| MailError::Address(e.to_string()))?;
    let email = Message::builder()
        .from(from)
        .to(to)
        .subject(msg.subject.clone())
        .multipart(
            MultiPart::alternative()
                .singlepart(SinglePart::builder().header(ContentType::TEXT_PLAIN).body(msg.text(&cfg.app_name)))
                .singlepart(SinglePart::builder().header(ContentType::TEXT_HTML).body(msg.html(&cfg.app_name))),
        )
        .map_err(|e| MailError::Transport(e.to_string()))?;
    if cfg.mailer == "sendmail" {
        return AsyncSendmailTransport::<Tokio1Executor>::new()
            .send(email)
            .await
            .map(|_| ())
            .map_err(|e| MailError::Transport(e.to_string()));
    }
    let builder = if matches!(cfg.scheme.as_str(), "smtps" | "ssl" | "tls") {
        AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.host).map_err(|e| MailError::Transport(e.to_string()))?
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.host)
            .map(|b| b)
            .unwrap_or_else(|_| AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&cfg.host))
    };
    let mut builder = builder.port(cfg.port);
    if let (Some(u), Some(p)) = (&cfg.username, &cfg.password) {
        builder = builder.credentials(Credentials::new(u.clone(), p.clone()));
    }
    builder.build().send(email).await.map(|_| ()).map_err(|e| MailError::Transport(e.to_string()))
}
