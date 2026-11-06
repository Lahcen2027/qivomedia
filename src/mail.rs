//! Outgoing email (password resets, invitations, lead notifications).
//!
//! Configure with SMTP_HOST, SMTP_PORT, SMTP_USER, SMTP_PASSWORD, SMTP_FROM and
//! SMTP_TLS (starttls | tls | none). Without SMTP_HOST nothing is sent: the message
//! is written to the server log instead, so a self-hosted admin can still copy a
//! reset link from `qivo logs qivomedia`.

use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};

use lettre::{
    message::{header::ContentType, Mailbox, MultiPart, SinglePart},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};

#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub text: String,
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

pub fn configured() -> bool {
    env("SMTP_HOST").is_some()
}

/// Messages not sent because SMTP is not configured (kept in memory for tests and debugging).
fn outbox() -> &'static Mutex<VecDeque<Mail>> {
    static O: OnceLock<Mutex<VecDeque<Mail>>> = OnceLock::new();
    O.get_or_init(Default::default)
}

#[cfg(test)]
pub fn unsent() -> Vec<Mail> {
    outbox().lock().unwrap().iter().cloned().collect()
}

fn transport() -> anyhow::Result<AsyncSmtpTransport<Tokio1Executor>> {
    let host = env("SMTP_HOST").ok_or_else(|| anyhow::anyhow!("SMTP_HOST not set"))?;
    let tls = env("SMTP_TLS").unwrap_or_else(|| "starttls".into());
    let mut b = match tls.as_str() {
        "tls" => AsyncSmtpTransport::<Tokio1Executor>::relay(&host)?,
        "none" => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&host),
        _ => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&host)?,
    };
    if let Some(port) = env("SMTP_PORT").and_then(|p| p.parse::<u16>().ok()) {
        b = b.port(port);
    }
    if let (Some(u), Some(p)) = (env("SMTP_USER"), env("SMTP_PASSWORD")) {
        b = b.credentials(Credentials::new(u, p));
    }
    Ok(b.timeout(Some(std::time::Duration::from_secs(20))).build())
}

/// Plain branded HTML wrapper; `body_html` must already be escaped.
fn wrap_html(title: &str, body_html: &str) -> String {
    format!(
        r#"<!doctype html><html><body style="margin:0;background:#f1f3f8;font-family:Segoe UI,Roboto,Arial,sans-serif;color:#141926">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0"><tr><td align="center" style="padding:32px 16px">
<table role="presentation" width="560" cellpadding="0" cellspacing="0" style="max-width:560px;background:#ffffff;border-radius:14px;border:1px solid #dfe3ec">
<tr><td style="padding:28px 32px 8px"><span style="display:inline-block;width:22px;height:22px;border-radius:6px;background:#6a4cf0;vertical-align:middle"></span>
<b style="font-size:18px;vertical-align:middle;margin-left:8px">Qivomedia</b></td></tr>
<tr><td style="padding:8px 32px 28px"><h1 style="font-size:20px;margin:12px 0 14px">{title}</h1>{body_html}</td></tr>
</table><p style="font-size:12px;color:#8c96ab;margin:16px 0 0">Qivomedia · real-time digital signage</p></td></tr></table></body></html>"#
    )
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Button-style link block for emails.
pub fn button(url: &str, label: &str) -> String {
    format!(
        r#"<p style="margin:22px 0"><a href="{u}" style="background:#6a4cf0;color:#ffffff;text-decoration:none;font-weight:600;padding:12px 20px;border-radius:10px;display:inline-block">{l}</a></p>
<p style="font-size:13px;color:#4b5468">Or copy this link: <br><a href="{u}" style="color:#5b3fd6;word-break:break-all">{u}</a></p>"#,
        u = esc(url),
        l = esc(label)
    )
}

/// Send (or log, when SMTP isn't configured). Never fails the caller's request:
/// errors are logged and returned for callers that care.
pub async fn send(to: &str, subject: &str, text: &str, body_html: &str) -> anyhow::Result<()> {
    if !configured() {
        tracing::warn!("email NOT sent (SMTP not configured) to={to} subject={subject:?}\n{text}");
        let mut o = outbox().lock().unwrap();
        o.push_back(Mail { to: to.into(), subject: subject.into(), text: text.into() });
        while o.len() > 50 {
            o.pop_front();
        }
        return Ok(());
    }
    let from: Mailbox = env("SMTP_FROM")
        .unwrap_or_else(|| "Qivomedia <no-reply@qivomedia.com>".into())
        .parse()?;
    let msg = Message::builder()
        .from(from)
        .to(to.parse()?)
        .subject(subject)
        .multipart(
            MultiPart::alternative()
                .singlepart(SinglePart::builder().header(ContentType::TEXT_PLAIN).body(text.to_string()))
                .singlepart(SinglePart::builder().header(ContentType::TEXT_HTML).body(wrap_html(&esc(subject), body_html))),
        )?;
    match transport()?.send(msg).await {
        Ok(_) => {
            tracing::info!("email sent to={to} subject={subject:?}");
            Ok(())
        }
        Err(e) => {
            tracing::error!("email to {to} failed: {e}");
            Err(e.into())
        }
    }
}

/// Fire-and-forget variant for request handlers.
pub fn send_bg(to: String, subject: String, text: String, html: String) {
    tokio::spawn(async move {
        let _ = send(&to, &subject, &text, &html).await;
    });
}
