//! Password policy following NIST SP 800-63B: length over complexity rules, a
//! blocklist of common and context-specific passwords, and a breached-password
//! check against Have I Been Pwned using k-anonymity (only the first 5 hex chars
//! of the SHA-1 leave the server). The online check fails open so an outage
//! never blocks sign-in flows; disable it with QIVO_HIBP=0.

use sha1_smol::Sha1;

use crate::error::{AppError, AppResult};

pub const MIN_LEN: usize = 12;
pub const MAX_LEN: usize = 128;

/// The most common passwords (normalized to lower case) plus signage-specific ones.
const COMMON: &[&str] = &[
    "123456789012", "1234567890123", "password1234", "password12345", "passwordpassword", "qwertyuiopas",
    "qwerty123456", "123456123456", "111111111111", "000000000000", "abc123abc123", "iloveyou1234",
    "administrator", "admin1234567", "adminadmin12", "welcome12345", "welcome123456", "letmein12345",
    "changeme1234", "changemenow!", "motdepasse12", "motdepasse123", "azerty123456", "azertyuiopqs",
    "bonjour12345", "soleil123456", "maroc1234567", "morocco12345", "casablanca12", "rabat1234567",
    "qivomedia123", "qivomedia1234", "qivomedia2026", "signage12345", "digitalsignage", "xibosignage1",
    "password!234", "p@ssword1234", "p@ssw0rd1234", "passw0rd1234", "1q2w3e4r5t6y", "zaq12wsxcde3",
    "football1234", "baseball1234", "sunshine1234", "princess1234", "superman1234", "dragon123456",
    "monkey123456", "trustno11234", "starwars1234", "whatever1234", "computer1234", "internet1234",
];

fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Same character repeated, or a straight run like "abcdefghijkl" / "123456789012".
fn trivially_patterned(pw: &str) -> bool {
    let c: Vec<char> = pw.chars().collect();
    if c.windows(2).all(|w| w[0] == w[1]) {
        return true;
    }
    let up = c.windows(2).all(|w| (w[1] as i64) - (w[0] as i64) == 1);
    let down = c.windows(2).all(|w| (w[0] as i64) - (w[1] as i64) == 1);
    up || down
}

/// Offline checks (fast, always on). `context` holds things the password must not
/// be built from: email, name, organization.
pub fn check_local(pw: &str, context: &[&str]) -> AppResult<()> {
    let n = pw.chars().count();
    if n < MIN_LEN {
        return Err(AppError::BadRequest(format!("use at least {MIN_LEN} characters — a short phrase of 3–4 words works well")));
    }
    if n > MAX_LEN {
        return Err(AppError::BadRequest(format!("passwords can be at most {MAX_LEN} characters")));
    }
    let low = normalize(pw);
    if COMMON.contains(&low.as_str()) || trivially_patterned(pw) {
        return Err(AppError::BadRequest("this password is too common — choose something less predictable".into()));
    }
    for c in context {
        let c = normalize(c);
        let base = c.split('@').next().unwrap_or("");
        for part in [c.as_str(), base] {
            if part.chars().count() >= 4 && low.contains(part) {
                return Err(AppError::BadRequest("don't build your password from your name, email or organization".into()));
            }
        }
    }
    Ok(())
}

/// Have I Been Pwned range check. Ok(true) = found in breaches; Ok(false) = not found
/// or the check could not run (fail open).
pub async fn is_breached(pw: &str) -> bool {
    if std::env::var("QIVO_HIBP").map(|v| v == "0" || v == "false").unwrap_or(false) {
        return false;
    }
    let hash = Sha1::from(pw.as_bytes()).digest().to_string().to_uppercase();
    let (prefix, suffix) = hash.split_at(5);
    let client = match reqwest::Client::builder().timeout(std::time::Duration::from_secs(3)).build() {
        Ok(c) => c,
        Err(_) => return false,
    };
    let res = client
        .get(format!("https://api.pwnedpasswords.com/range/{prefix}"))
        .header("Add-Padding", "true")
        .header("User-Agent", "Qivomedia-password-check")
        .send()
        .await;
    match res {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(body) => body.lines().any(|l| {
                let mut it = l.trim().split(':');
                matches!((it.next(), it.next()), (Some(s), Some(count)) if s == suffix && count.trim() != "0")
            }),
            Err(_) => false,
        },
        Ok(r) => {
            tracing::warn!("breached-password check unavailable: HTTP {}", r.status());
            false
        }
        Err(e) => {
            tracing::warn!("breached-password check unavailable: {e}");
            false
        }
    }
}

/// Full policy for a new password.
pub async fn check_new(pw: &str, context: &[&str]) -> AppResult<()> {
    check_local(pw, context)?;
    if is_breached(pw).await {
        return Err(AppError::BadRequest(
            "this password appears in known data breaches — please choose a different one".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy() {
        assert!(check_local("short", &[]).is_err());
        assert!(check_local("Password1234", &[]).is_err(), "common list is case-insensitive");
        assert!(check_local("aaaaaaaaaaaaaa", &[]).is_err());
        assert!(check_local("abcdefghijklm", &[]).is_err());
        assert!(check_local("amina-likes-tea", &["amina@fs.ma"]).is_err());
        assert!(check_local("correct horse battery", &["amina@fs.ma", "Faculty of Sciences"]).is_ok());
        assert!(check_local(&"x".repeat(129), &[]).is_err());
    }
}
