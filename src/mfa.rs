//! Two-factor authentication with authenticator apps (TOTP, RFC 6238) plus one-time
//! recovery codes. Codes are accepted for the current 30-second step and one step
//! either side (clock drift), and never twice (replay protection via the last used
//! step). Second-step tickets live 5 minutes and allow 5 attempts.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use axum::{
    extract::State,
    http::HeaderMap,
    response::Response,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use totp_rs::{Algorithm, Secret, TOTP};

use crate::{
    auth::{audit, user_from_token, who, with_session, AuthUser},
    error::{AppError, AppResult},
    ratelimit::{self, client_ip, HOUR},
    session,
    state::AppState,
    util::{random_token, sha256_hex},
};

const TICKET_TTL: Duration = Duration::from_secs(300);
const TICKET_ATTEMPTS: u8 = 5;
const RECOVERY_CODES: usize = 10;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/mfa/setup", post(setup))
        .route("/auth/mfa/enable", post(enable))
        .route("/auth/mfa/disable", post(disable))
        .route("/auth/mfa/recovery-codes", post(regenerate_codes))
        .route("/auth/mfa/verify", post(verify))
        .route("/org/security", get(org_security).patch(set_org_security))
}

// ------------------------------------------------------------ tickets (between password and code)

struct Ticket {
    user_id: String,
    created: Instant,
    attempts: u8,
}

fn tickets() -> &'static Mutex<HashMap<String, Ticket>> {
    static T: OnceLock<Mutex<HashMap<String, Ticket>>> = OnceLock::new();
    T.get_or_init(Default::default)
}

pub fn issue_ticket(user_id: &str) -> String {
    let t = random_token(40);
    let mut m = tickets().lock().unwrap();
    m.retain(|_, v| v.created.elapsed() < TICKET_TTL);
    m.insert(sha256_hex(&t), Ticket { user_id: user_id.into(), created: Instant::now(), attempts: 0 });
    t
}

/// Count an attempt; returns the user id while the ticket is still valid.
fn use_ticket(t: &str) -> Option<String> {
    let mut m = tickets().lock().unwrap();
    let key = sha256_hex(t);
    let tk = m.get_mut(&key)?;
    if tk.created.elapsed() >= TICKET_TTL || tk.attempts >= TICKET_ATTEMPTS {
        m.remove(&key);
        return None;
    }
    tk.attempts += 1;
    Some(tk.user_id.clone())
}

fn burn_ticket(t: &str) {
    tickets().lock().unwrap().remove(&sha256_hex(t));
}

// ------------------------------------------------------------ TOTP helpers

fn totp_for(secret_b32: &str, account: &str) -> AppResult<TOTP> {
    let bytes = Secret::Encoded(secret_b32.to_string())
        .to_bytes()
        .map_err(|_| AppError::Other(anyhow::anyhow!("bad TOTP secret")))?;
    TOTP::new(Algorithm::SHA1, 6, 1, 30, bytes, Some("Qivomedia".into()), account.to_string())
        .map_err(|e| AppError::Other(anyhow::anyhow!("totp: {e}")))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Returns the matched time step if `code` is valid and newer than `last_step`.
fn check_code(totp: &TOTP, code: &str, last_step: i64) -> Option<i64> {
    let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
    if code.len() != 6 {
        return None;
    }
    let now = now_secs();
    for t in [now.saturating_sub(30), now, now + 30] {
        let step = (t / 30) as i64;
        if step > last_step && ct_eq(&totp.generate(t), &code) {
            return Some(step);
        }
    }
    None
}

fn new_recovery_codes() -> (Vec<String>, String) {
    const A: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let codes: Vec<String> = (0..RECOVERY_CODES)
        .map(|_| {
            let s: String = (0..10).map(|_| A[rng.gen_range(0..A.len())] as char).collect();
            format!("{}-{}", &s[..5], &s[5..])
        })
        .collect();
    let hashes: Vec<String> = codes.iter().map(|c| sha256_hex(c)).collect();
    (codes, serde_json::to_string(&hashes).unwrap())
}

fn normalize_recovery(c: &str) -> String {
    let s: String = c.trim().to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if s.len() == 10 { format!("{}-{}", &s[..5], &s[5..]) } else { s }
}

struct MfaState {
    email: String,
    org_id: String,
    enabled: bool,
    secret: Option<String>,
    pending: Option<String>,
    recovery: String,
    last_step: i64,
}

async fn load(state: &AppState, user_id: &str) -> AppResult<MfaState> {
    let r: (String, String, bool, Option<String>, Option<String>, String, i64) = sqlx::query_as(
        "SELECT email, org_id, mfa_enabled, mfa_secret, mfa_pending_secret, mfa_recovery, mfa_last_step FROM users WHERE id = ?",
    )
    .bind(user_id)
    .fetch_one(&state.db)
    .await?;
    Ok(MfaState { email: r.0, org_id: r.1, enabled: r.2, secret: r.3, pending: r.4, recovery: r.5, last_step: r.6 })
}

/// Check a TOTP or recovery code for an enrolled user, consuming what it uses.
async fn verify_second_factor(state: &AppState, user_id: &str, code: &str) -> AppResult<bool> {
    let m = load(state, user_id).await?;
    let Some(secret) = m.secret.filter(|_| m.enabled) else { return Ok(false) };
    if let Some(step) = check_code(&totp_for(&secret, &m.email)?, code, m.last_step) {
        sqlx::query("UPDATE users SET mfa_last_step = ? WHERE id = ?").bind(step).bind(user_id).execute(&state.db).await?;
        return Ok(true);
    }
    let mut hashes: Vec<String> = serde_json::from_str(&m.recovery).unwrap_or_default();
    let h = sha256_hex(&normalize_recovery(code));
    if let Some(i) = hashes.iter().position(|x| ct_eq(x, &h)) {
        hashes.remove(i);
        sqlx::query("UPDATE users SET mfa_recovery = ? WHERE id = ?")
            .bind(serde_json::to_string(&hashes).unwrap()).bind(user_id).execute(&state.db).await?;
        audit(state, &m.org_id, Some(user_id), "auth.mfa_recovery_code_used", &format!("{} left", hashes.len())).await;
        return Ok(true);
    }
    Ok(false)
}

// ------------------------------------------------------------ enrollment

async fn setup(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    let secret = match Secret::generate_secret().to_encoded() {
        Secret::Encoded(s) => s,
        Secret::Raw(_) => return Err(AppError::Other(anyhow::anyhow!("secret encoding"))),
    };
    let totp = totp_for(&secret, &user.0.email)?;
    sqlx::query("UPDATE users SET mfa_pending_secret = ? WHERE id = ?").bind(&secret).bind(&user.0.id).execute(&state.db).await?;
    let qr = totp.get_qr_base64().map_err(|e| AppError::Other(anyhow::anyhow!("qr: {e}")))?;
    Ok(Json(json!({ "secret": secret, "otpauth_url": totp.get_url(), "qr_png_base64": qr })))
}

#[derive(Deserialize)]
struct CodeReq {
    code: String,
}

async fn enable(State(state): State<AppState>, headers: HeaderMap, user: AuthUser, Json(r): Json<CodeReq>) -> AppResult<Json<Value>> {
    if !ratelimit::allow(&format!("mfa:enable:{}", user.0.id), 10, HOUR) {
        return Err(AppError::TooMany("too many attempts — try again later".into()));
    }
    let m = load(&state, &user.0.id).await?;
    if m.enabled {
        return Err(AppError::Conflict("two-factor authentication is already on".into()));
    }
    let pending = m.pending.ok_or_else(|| AppError::BadRequest("start the setup first".into()))?;
    let step = check_code(&totp_for(&pending, &m.email)?, &r.code, 0)
        .ok_or_else(|| AppError::BadRequest("that code didn't match — check your phone's clock and try the newest code".into()))?;
    let (codes, hashes) = new_recovery_codes();
    sqlx::query("UPDATE users SET mfa_enabled = 1, mfa_secret = ?, mfa_pending_secret = NULL, mfa_recovery = ?, mfa_last_step = ? WHERE id = ?")
        .bind(&pending).bind(&hashes).bind(step).bind(&user.0.id)
        .execute(&state.db).await?;
    // Other sessions were created without the second factor: sign them out.
    let current = session::token_from_headers(&headers).map(|t| sha256_hex(&t)).unwrap_or_default();
    sqlx::query("DELETE FROM sessions WHERE user_id = ? AND token_hash != ?").bind(&user.0.id).bind(&current).execute(&state.db).await?;
    sqlx::query("UPDATE sessions SET mfa = 1 WHERE token_hash = ?").bind(&current).execute(&state.db).await?;
    audit(&state, user.org(), Some(&user.0.id), "auth.mfa_enabled", &who(&headers)).await;
    Ok(Json(json!({ "ok": true, "recovery_codes": codes })))
}

#[derive(Deserialize)]
struct DisableReq {
    password: String,
    code: String,
}

async fn disable(State(state): State<AppState>, headers: HeaderMap, user: AuthUser, Json(r): Json<DisableReq>) -> AppResult<Json<Value>> {
    if !ratelimit::allow(&format!("mfa:disable:{}", user.0.id), 10, HOUR) {
        return Err(AppError::TooMany("too many attempts — try again later".into()));
    }
    let require: bool = sqlx::query_scalar("SELECT require_mfa FROM organizations WHERE id = ?").bind(user.org()).fetch_one(&state.db).await?;
    if require {
        return Err(AppError::Forbidden("your organization requires two-factor authentication".into()));
    }
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?").bind(&user.0.id).fetch_one(&state.db).await?;
    if !crate::auth::verify_password_pub(&r.password, &hash) || !verify_second_factor(&state, &user.0.id, &r.code).await? {
        return Err(AppError::BadRequest("password or code is incorrect".into()));
    }
    sqlx::query("UPDATE users SET mfa_enabled = 0, mfa_secret = NULL, mfa_pending_secret = NULL, mfa_recovery = '[]' WHERE id = ?")
        .bind(&user.0.id).execute(&state.db).await?;
    audit(&state, user.org(), Some(&user.0.id), "auth.mfa_disabled", &who(&headers)).await;
    Ok(Json(json!({ "ok": true })))
}

async fn regenerate_codes(State(state): State<AppState>, user: AuthUser, Json(r): Json<CodeReq>) -> AppResult<Json<Value>> {
    if !ratelimit::allow(&format!("mfa:codes:{}", user.0.id), 10, HOUR) {
        return Err(AppError::TooMany("too many attempts — try again later".into()));
    }
    if !verify_second_factor(&state, &user.0.id, &r.code).await? {
        return Err(AppError::BadRequest("that code didn't match".into()));
    }
    let (codes, hashes) = new_recovery_codes();
    sqlx::query("UPDATE users SET mfa_recovery = ? WHERE id = ?").bind(&hashes).bind(&user.0.id).execute(&state.db).await?;
    audit(&state, user.org(), Some(&user.0.id), "auth.mfa_recovery_codes_regenerated", "").await;
    Ok(Json(json!({ "recovery_codes": codes })))
}

// ------------------------------------------------------------ second step of sign-in

#[derive(Deserialize)]
struct VerifyReq {
    ticket: String,
    code: String,
}

async fn verify(State(state): State<AppState>, headers: HeaderMap, Json(r): Json<VerifyReq>) -> AppResult<Response> {
    if !ratelimit::allow(&format!("mfa:verify:ip:{}", client_ip(&headers)), 30, HOUR) {
        return Err(AppError::TooMany("too many attempts — try again later".into()));
    }
    let uid = use_ticket(&r.ticket).ok_or_else(|| AppError::BadRequest("your sign-in expired — please enter your password again".into()))?;
    if !verify_second_factor(&state, &uid, &r.code).await? {
        let org: String = sqlx::query_scalar("SELECT org_id FROM users WHERE id = ?").bind(&uid).fetch_one(&state.db).await?;
        audit(&state, &org, Some(&uid), "auth.mfa_failed", &who(&headers)).await;
        return Err(AppError::BadRequest("that code didn't match".into()));
    }
    burn_ticket(&r.ticket);
    let (token, cookie) = session::create(&state, &uid, &headers, true).await?;
    let user = user_from_token(&state, &token).await?;
    audit(&state, &user.org_id, Some(&uid), "auth.login", &format!("{} · with 2FA", who(&headers))).await;
    Ok(with_session(cookie, json!({ "user": user })))
}

// ------------------------------------------------------------ recovery (lost phone)

/// Remove a user's authenticator and recovery codes and sign them out everywhere.
/// Used by organization admins, the platform admin, and the `reset-mfa` server command.
/// Returns (email, name, org_id).
pub async fn reset_for_user(db: &sqlx::SqlitePool, user_id: &str) -> AppResult<(String, String, String)> {
    let row: Option<(String, String, String)> = sqlx::query_as("SELECT email, name, org_id FROM users WHERE id = ?")
        .bind(user_id).fetch_optional(db).await?;
    let (email, name, org) = row.ok_or(AppError::NotFound)?;
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE users SET mfa_enabled = 0, mfa_secret = NULL, mfa_pending_secret = NULL, mfa_recovery = '[]', mfa_last_step = 0 WHERE id = ?")
        .bind(user_id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = ?").bind(user_id).execute(&mut *tx).await?;
    tx.commit().await?;
    ratelimit::reset(&format!("login:acct:{email}"));
    Ok((email, name, org))
}

/// Tell the person their 2FA was reset, so an unexpected reset doesn't go unnoticed.
pub async fn notify_reset(email: &str, name: &str, by: &str) {
    let text = format!(
        "Hello {name},\n\nTwo-factor authentication on your Qivomedia account was reset by {by}, and you were signed out everywhere.\n\nNext time you sign in with your password you can set it up again with your new phone.\n\nIf you did not ask for this, contact your administrator immediately and change your password.\n"
    );
    let html = format!(
        "<p>Hello {},</p><p>Two-factor authentication on your Qivomedia account was <b>reset by {}</b>, and you were signed out everywhere.</p><p>Next time you sign in with your password you can set it up again with your new phone.</p><p style=\"color:#8a1c1c\"><b>If you did not ask for this</b>, contact your administrator immediately and change your password.</p>",
        crate::mail::esc(name), crate::mail::esc(by)
    );
    let _ = crate::mail::send(email, "Your two-factor authentication was reset", &text, &html).await;
}

// ------------------------------------------------------------ organization policy

async fn org_security(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    let require: bool = sqlx::query_scalar("SELECT require_mfa FROM organizations WHERE id = ?").bind(user.org()).fetch_one(&state.db).await?;
    let (total, enrolled): (i64, i64) = sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(mfa_enabled),0) FROM users WHERE org_id = ?")
        .bind(user.org()).fetch_one(&state.db).await?;
    Ok(Json(json!({ "require_mfa": require, "members": total, "members_with_mfa": enrolled })))
}

#[derive(Deserialize)]
struct OrgSecReq {
    require_mfa: bool,
}

async fn set_org_security(State(state): State<AppState>, headers: HeaderMap, user: AuthUser, Json(r): Json<OrgSecReq>) -> AppResult<Json<Value>> {
    user.require_admin()?;
    if r.require_mfa && !user.0.mfa_enabled {
        return Err(AppError::BadRequest("turn on two-factor authentication for your own account first".into()));
    }
    sqlx::query("UPDATE organizations SET require_mfa = ? WHERE id = ?").bind(r.require_mfa).bind(user.org()).execute(&state.db).await?;
    audit(&state, user.org(), Some(&user.0.id), if r.require_mfa { "org.mfa_required" } else { "org.mfa_optional" }, &who(&headers)).await;
    Ok(Json(json!({ "require_mfa": r.require_mfa })))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codes_and_replay() {
        let secret = match Secret::generate_secret().to_encoded() { Secret::Encoded(s) => s, _ => unreachable!() };
        let t = totp_for(&secret, "a@b.co").unwrap();
        let now = now_secs();
        let code = t.generate(now);
        let step = check_code(&t, &code, 0).expect("current code accepted");
        assert!(check_code(&t, &code, step).is_none(), "same code can't be used twice");
        assert!(check_code(&t, "000000x", 0).is_none());
        let (codes, hashes) = new_recovery_codes();
        assert_eq!(codes.len(), RECOVERY_CODES);
        let hs: Vec<String> = serde_json::from_str(&hashes).unwrap();
        assert!(hs.contains(&sha256_hex(&normalize_recovery(&codes[0].to_uppercase().replace('-', " ")))));
    }
}
