//! Browser sessions. The session token lives only in an HttpOnly cookie (never in
//! JavaScript-readable storage or URLs), named `__Host-qivo_session` over HTTPS so
//! it is bound to the exact host with Secure + Path=/. SameSite=Strict keeps it off
//! cross-site requests; the CSRF guard in `headers.rs` adds an Origin check.
//!
//! Sessions expire after 30 days, or earlier after QIVO_SESSION_IDLE_HOURS
//! (default 12) without use.

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    auth::{audit, AuthUser},
    error::{AppError, AppResult},
    ratelimit::client_ip,
    state::AppState,
    util::{now, random_token, sha256_hex},
};

pub const COOKIE_SECURE: &str = "__Host-qivo_session";
pub const COOKIE_PLAIN: &str = "qivo_session";
pub const ABSOLUTE_DAYS: i64 = 30;

pub fn idle_hours() -> i64 {
    std::env::var("QIVO_SESSION_IDLE_HOURS").ok().and_then(|v| v.parse().ok()).filter(|h| *h > 0).unwrap_or(12)
}

/// Is the browser talking to us over HTTPS (directly or through the proxy)?
pub fn is_https(h: &HeaderMap) -> bool {
    if std::env::var("QIVO_SECURE_COOKIES").map(|v| v == "1" || v == "true").unwrap_or(false) {
        return true;
    }
    h.get("x-forwarded-proto").and_then(|v| v.to_str().ok()).map(|p| p.eq_ignore_ascii_case("https")).unwrap_or(false)
}

fn cookie_name(h: &HeaderMap) -> &'static str {
    if is_https(h) { COOKIE_SECURE } else { COOKIE_PLAIN }
}

pub fn set_cookie(token: &str, h: &HeaderMap) -> HeaderValue {
    let secure = if is_https(h) { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{secure}",
        cookie_name(h),
        ABSOLUTE_DAYS * 86_400
    ))
    .expect("valid cookie")
}

pub fn clear_cookies(h: &HeaderMap) -> Vec<HeaderValue> {
    let secure = if is_https(h) { "; Secure" } else { "" };
    vec![
        HeaderValue::from_str(&format!("{COOKIE_SECURE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0; Secure")).unwrap(),
        HeaderValue::from_str(&format!("{COOKIE_PLAIN}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure}")).unwrap(),
    ]
}

/// Session token from the cookie, or `Authorization: Bearer` for API clients.
/// Tokens in URLs are deliberately not accepted (they leak into logs and history).
pub fn token_from_headers(h: &HeaderMap) -> Option<String> {
    if let Some(t) = h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")) {
        return Some(t.trim().to_string());
    }
    for raw in h.get_all(header::COOKIE) {
        let Ok(s) = raw.to_str() else { continue };
        for c in cookie::Cookie::split_parse(s).flatten() {
            if (c.name() == COOKIE_SECURE || c.name() == COOKIE_PLAIN) && !c.value().is_empty() {
                return Some(c.value().to_string());
            }
        }
    }
    None
}


/// Create a session and return (token, Set-Cookie header).
pub async fn create(state: &AppState, user_id: &str, h: &HeaderMap, mfa: bool) -> AppResult<(String, HeaderValue)> {
    let token = random_token(48);
    let exp = (chrono::Utc::now() + chrono::Duration::days(ABSOLUTE_DAYS)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let ua: String = h.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").chars().take(200).collect();
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, expires_at, id, created_at, last_used_at, ip, user_agent, mfa)
         VALUES (?,?,?,?,?,?,?,?,?)",
    )
    .bind(sha256_hex(&token)).bind(user_id).bind(exp).bind(random_token(16)).bind(now()).bind(now())
    .bind(client_ip(h)).bind(ua).bind(mfa)
    .execute(&state.db).await?;
    sqlx::query("DELETE FROM sessions WHERE expires_at < ?").bind(now()).execute(&state.db).await?;
    let cookie = set_cookie(&token, h);
    Ok((token, cookie))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/sessions", get(list))
        .route("/auth/sessions/{id}", delete(revoke))
        .route("/auth/sessions/revoke-others", post(revoke_others))
}

#[derive(Serialize, sqlx::FromRow)]
struct SessionRow {
    id: String,
    token_hash: String,
    created_at: Option<String>,
    last_used_at: Option<String>,
    ip: Option<String>,
    user_agent: Option<String>,
    mfa: bool,
}

fn current_hash(h: &HeaderMap) -> String {
    token_from_headers(h).map(|t| sha256_hex(&t)).unwrap_or_default()
}

async fn list(State(state): State<AppState>, headers: HeaderMap, user: AuthUser) -> AppResult<Json<Vec<Value>>> {
    let rows = sqlx::query_as::<_, SessionRow>(
        "SELECT id, token_hash, created_at, last_used_at, ip, user_agent, mfa FROM sessions
          WHERE user_id = ? AND expires_at > ? ORDER BY last_used_at DESC",
    )
    .bind(&user.0.id).bind(now())
    .fetch_all(&state.db).await?;
    let cur = current_hash(&headers);
    Ok(Json(rows.into_iter().map(|r| json!({
        "id": r.id, "created_at": r.created_at, "last_used_at": r.last_used_at, "ip": r.ip,
        "user_agent": r.user_agent, "mfa": r.mfa, "current": r.token_hash == cur,
    })).collect()))
}

async fn revoke(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let r = sqlx::query("DELETE FROM sessions WHERE id = ? AND user_id = ?").bind(&id).bind(&user.0.id).execute(&state.db).await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    audit(&state, user.org(), Some(&user.0.id), "auth.session_revoked", &id).await;
    Ok(Json(json!({ "ok": true })))
}

async fn revoke_others(State(state): State<AppState>, headers: HeaderMap, user: AuthUser) -> AppResult<Json<Value>> {
    let r = sqlx::query("DELETE FROM sessions WHERE user_id = ? AND token_hash != ?")
        .bind(&user.0.id).bind(current_hash(&headers)).execute(&state.db).await?;
    audit(&state, user.org(), Some(&user.0.id), "auth.sessions_revoked", &format!("{} other session(s)", r.rows_affected())).await;
    Ok(Json(json!({ "revoked": r.rows_affected() })))
}
