use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{FromRequestParts, Path, State},
    http::request::Parts,
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    mail,
    ratelimit::{self, client_ip, HOUR},
    state::AppState,
    util::{new_id, now, random_token, sha256_hex},
};

/// Failed logins allowed per account (and per IP) before a 15-minute lockout.
const MAX_FAILED: usize = 5;
const LOCKOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

const SESSION_DAYS: i64 = 30;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct User {
    pub id: String,
    pub org_id: String,
    pub email: String,
    pub name: String,
    pub role: String,
    pub created_at: String,
    pub platform_admin: bool,
}

/// Authenticated dashboard user, extracted from `Authorization: Bearer <token>`
/// (or `?token=` for WebSocket upgrades).
#[derive(Debug, Clone)]
pub struct AuthUser(pub User);

impl AuthUser {
    fn rank(&self) -> u8 {
        match self.0.role.as_str() {
            "owner" => 3,
            "admin" => 2,
            "editor" => 1,
            _ => 0,
        }
    }
    pub fn org(&self) -> &str {
        &self.0.org_id
    }
    pub fn require_editor(&self) -> AppResult<()> {
        if self.rank() >= 1 { Ok(()) } else { Err(AppError::Forbidden("editor role required".into())) }
    }
    pub fn require_admin(&self) -> AppResult<()> {
        if self.rank() >= 2 { Ok(()) } else { Err(AppError::Forbidden("admin role required".into())) }
    }
    pub fn require_platform(&self) -> AppResult<()> {
        if self.0.platform_admin { Ok(()) } else { Err(AppError::Forbidden("platform admin only".into())) }
    }
}

pub async fn user_from_token(state: &AppState, token: &str) -> AppResult<User> {
    let row: Option<(User, bool)> = sqlx::query_as::<_, (String, String, String, String, String, String, bool, bool)>(
        "SELECT u.id, u.org_id, u.email, u.name, u.role, u.created_at, u.platform_admin, o.suspended
           FROM sessions s JOIN users u ON u.id = s.user_id JOIN organizations o ON o.id = u.org_id
          WHERE s.token_hash = ? AND s.expires_at > ?",
    )
    .bind(sha256_hex(token))
    .bind(now())
    .fetch_optional(&state.db)
    .await?
    .map(|(id, org_id, email, name, role, created_at, platform_admin, suspended)| {
        (User { id, org_id, email, name, role, created_at, platform_admin }, suspended)
    });
    let (user, suspended) = row.ok_or(AppError::Unauthorized)?;
    if suspended && !user.platform_admin {
        return Err(AppError::Forbidden("this account is suspended — contact your provider".into()));
    }
    Ok(user)
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let header_token = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_owned);
        let query_token = parts.uri.query().and_then(|q| {
            q.split('&').find_map(|kv| kv.strip_prefix("token=").map(str::to_owned))
        });
        let token = header_token.or(query_token).ok_or(AppError::Unauthorized)?;
        Ok(AuthUser(user_from_token(state, &token).await?))
    }
}

pub fn hash_password(pw: &str) -> AppResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::Other(anyhow::anyhow!("hash: {e}")))
}

fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

pub async fn audit(state: &AppState, org_id: &str, user_id: Option<&str>, action: &str, detail: &str) {
    let _ = sqlx::query("INSERT INTO audit_log (org_id, user_id, action, detail, created_at) VALUES (?,?,?,?,?)")
        .bind(org_id)
        .bind(user_id)
        .bind(action)
        .bind(detail)
        .bind(now())
        .execute(&state.db)
        .await;
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/status", get(status))
        .route("/auth/signup", post(signup))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
        .route("/auth/forgot", post(forgot))
        .route("/auth/reset", post(reset_password))
        .route("/auth/token-info", post(token_info))
        .route("/users", get(list_users).post(create_user))
        .route("/users/{id}", delete(delete_user).patch(update_user))
        .route("/audit", get(list_audit))
}

async fn user_count(state: &AppState) -> AppResult<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(&state.db).await?)
}

async fn status(State(state): State<AppState>) -> AppResult<Json<Value>> {
    let n = user_count(&state).await?;
    Ok(Json(json!({ "needs_setup": n == 0, "allow_signup": state.allow_signup || n == 0, "email": mail::configured() })))
}

#[derive(Deserialize)]
struct SignupReq {
    org_name: String,
    name: String,
    email: String,
    password: String,
}

fn validate_credentials(email: &str, password: &str) -> AppResult<()> {
    if !email.contains('@') || email.len() < 3 {
        return Err(AppError::BadRequest("valid email required".into()));
    }
    if password.len() < 8 {
        return Err(AppError::BadRequest("password must be at least 8 characters".into()));
    }
    Ok(())
}

async fn create_session(state: &AppState, user_id: &str) -> AppResult<String> {
    let token = random_token(48);
    let exp = (chrono::Utc::now() + chrono::Duration::days(SESSION_DAYS))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    sqlx::query("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES (?,?,?)")
        .bind(sha256_hex(&token))
        .bind(user_id)
        .bind(exp)
        .execute(&state.db)
        .await?;
    Ok(token)
}

async fn signup(State(state): State<AppState>, headers: axum::http::HeaderMap, Json(req): Json<SignupReq>) -> AppResult<Json<Value>> {
    if !ratelimit::allow(&format!("signup:{}", client_ip(&headers)), 10, HOUR) {
        return Err(AppError::TooMany("too many sign-ups from this network — try again later".into()));
    }
    let n = user_count(&state).await?;
    if n > 0 && !state.allow_signup {
        return Err(AppError::Forbidden("sign-up is disabled on this server".into()));
    }
    let email = req.email.trim().to_lowercase();
    validate_credentials(&email, &req.password)?;
    if req.org_name.trim().is_empty() || req.name.trim().is_empty() {
        return Err(AppError::BadRequest("organization and name are required".into()));
    }
    let org_id = new_id();
    let user_id = new_id();
    let hash = hash_password(&req.password)?;
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO organizations (id, name, created_at) VALUES (?,?,?)")
        .bind(&org_id)
        .bind(req.org_name.trim())
        .bind(now())
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO users (id, org_id, email, name, password_hash, role, created_at, platform_admin) VALUES (?,?,?,?,?,'owner',?,?)")
        .bind(&user_id)
        .bind(&org_id)
        .bind(&email)
        .bind(req.name.trim())
        .bind(hash)
        .bind(now())
        .bind(n == 0)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let token = create_session(&state, &user_id).await?;
    audit(&state, &org_id, Some(&user_id), "org.created", req.org_name.trim()).await;
    let user = user_from_token(&state, &token).await?;
    Ok(Json(json!({ "token": token, "user": user })))
}

#[derive(Deserialize)]
struct LoginReq {
    email: String,
    password: String,
}

async fn login(State(state): State<AppState>, headers: axum::http::HeaderMap, Json(req): Json<LoginReq>) -> AppResult<Json<Value>> {
    let email = req.email.trim().to_lowercase();
    let ip = client_ip(&headers);
    let (acct_key, ip_key) = (format!("login:acct:{email}"), format!("login:ip:{ip}"));
    // Lock an account after repeated failures, and an IP that tries many accounts.
    if ratelimit::exceeded(&acct_key, MAX_FAILED, LOCKOUT) || ratelimit::exceeded(&ip_key, MAX_FAILED * 6, LOCKOUT) {
        return Err(AppError::TooMany("too many failed attempts — wait 15 minutes or reset your password".into()));
    }
    let row: Option<(String, String)> = sqlx::query_as("SELECT id, password_hash FROM users WHERE email = ?")
        .bind(&email)
        .fetch_optional(&state.db)
        .await?;
    let Some((uid, hash)) = row else {
        // Spend comparable time to avoid user enumeration via timing.
        let _ = hash_password(&req.password);
        ratelimit::hit(&acct_key);
        ratelimit::hit(&ip_key);
        return Err(AppError::Unauthorized);
    };
    if !verify_password(&req.password, &hash) {
        ratelimit::hit(&acct_key);
        ratelimit::hit(&ip_key);
        return Err(AppError::Unauthorized);
    }
    ratelimit::reset(&acct_key);
    sqlx::query("DELETE FROM sessions WHERE expires_at < ?").bind(now()).execute(&state.db).await?;
    let token = create_session(&state, &uid).await?;
    let user = user_from_token(&state, &token).await?;
    Ok(Json(json!({ "token": token, "user": user })))
}

async fn logout(State(state): State<AppState>, parts: axum::http::HeaderMap) -> AppResult<Json<Value>> {
    if let Some(t) = parts
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?").bind(sha256_hex(t)).execute(&state.db).await?;
    }
    Ok(Json(json!({ "ok": true })))
}

async fn me(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    let org: (String, String, Option<i64>, bool) =
        sqlx::query_as("SELECT name, plan, max_screens, ai_enabled FROM organizations WHERE id = ?")
            .bind(user.org())
            .fetch_one(&state.db)
            .await?;
    Ok(Json(json!({
        "user": user.0,
        "organization": { "id": user.org(), "name": org.0, "plan": org.1, "max_screens": org.2 },
        "ai": { "available": crate::ai::configured() && org.3 },
    })))
}

async fn list_users(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Vec<User>>> {
    let users = sqlx::query_as::<_, User>(
        "SELECT id, org_id, email, name, role, created_at, platform_admin FROM users WHERE org_id = ? ORDER BY created_at",
    )
    .bind(user.org())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(users))
}

#[derive(Deserialize)]
struct CreateUserReq {
    email: String,
    name: String,
    /// Required unless `invite` is true.
    #[serde(default)]
    password: String,
    role: String,
    /// Email an invitation link instead of setting a password.
    #[serde(default)]
    invite: bool,
}

fn assignable_role(role: &str) -> AppResult<()> {
    match role {
        "admin" | "editor" | "viewer" => Ok(()),
        _ => Err(AppError::BadRequest("role must be admin, editor or viewer".into())),
    }
}

async fn create_user(State(state): State<AppState>, headers: axum::http::HeaderMap, user: AuthUser, Json(req): Json<CreateUserReq>) -> AppResult<Json<User>> {
    user.require_admin()?;
    let email = req.email.trim().to_lowercase();
    let password = if req.invite { random_token(32) } else { req.password.clone() };
    validate_credentials(&email, &password)?;
    assignable_role(&req.role)?;
    let id = new_id();
    sqlx::query("INSERT INTO users (id, org_id, email, name, password_hash, role, created_at) VALUES (?,?,?,?,?,?,?)")
        .bind(&id)
        .bind(user.org())
        .bind(&email)
        .bind(req.name.trim())
        .bind(hash_password(&password)?)
        .bind(&req.role)
        .bind(now())
        .execute(&state.db)
        .await?;
    audit(&state, user.org(), Some(&user.0.id), if req.invite { "user.invited" } else { "user.created" }, &email).await;
    if req.invite {
        let org_name: String = sqlx::query_scalar("SELECT name FROM organizations WHERE id = ?").bind(user.org()).fetch_one(&state.db).await?;
        let token = issue_token(&state, &id, "invite", chrono::Duration::days(7)).await?;
        let link = format!("{}/#/reset?token={token}", public_url(&headers));
        mail::send_bg(
            email.clone(),
            format!("{} invited you to {org_name} on Qivomedia", user.0.name),
            format!("{} invited you to manage digital signage for {org_name} on Qivomedia.\n\nChoose your password to get started (link valid for 7 days):\n{link}\n", user.0.name),
            format!("<p><b>{}</b> invited you to manage the screens of <b>{}</b> on Qivomedia.</p>{}<p style=\"font-size:13px;color:#4b5468\">This link is valid for 7 days.</p>",
                mail::esc(&user.0.name), mail::esc(&org_name), mail::button(&link, "Choose my password")),
        );
    }
    let u = sqlx::query_as::<_, User>("SELECT id, org_id, email, name, role, created_at, platform_admin FROM users WHERE id = ?")
        .bind(&id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(u))
}

#[derive(Deserialize)]
struct UpdateUserReq {
    role: Option<String>,
    name: Option<String>,
    password: Option<String>,
}

async fn update_user(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<UpdateUserReq>,
) -> AppResult<Json<Value>> {
    let is_self = id == user.0.id;
    if !is_self {
        user.require_admin()?;
    }
    let target_role: String = sqlx::query_scalar("SELECT role FROM users WHERE id = ? AND org_id = ?")
        .bind(&id)
        .bind(user.org())
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    if let Some(role) = &req.role {
        if is_self || target_role == "owner" {
            return Err(AppError::Forbidden("cannot change this user's role".into()));
        }
        assignable_role(role)?;
        sqlx::query("UPDATE users SET role = ? WHERE id = ?").bind(role).bind(&id).execute(&state.db).await?;
    }
    if let Some(name) = &req.name {
        sqlx::query("UPDATE users SET name = ? WHERE id = ?").bind(name.trim()).bind(&id).execute(&state.db).await?;
    }
    if let Some(pw) = &req.password {
        if pw.len() < 8 {
            return Err(AppError::BadRequest("password must be at least 8 characters".into()));
        }
        sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(hash_password(pw)?)
            .bind(&id)
            .execute(&state.db)
            .await?;
        // Sign out every other session of that user (keep the caller's own session).
        let current = bearer(&headers).map(|t| sha256_hex(&t)).unwrap_or_default();
        sqlx::query("DELETE FROM sessions WHERE user_id = ? AND token_hash != ?").bind(&id).bind(current).execute(&state.db).await?;
        audit(&state, user.org(), Some(&user.0.id), "user.password_changed", &id).await;
    }
    Ok(Json(json!({ "ok": true })))
}

async fn delete_user(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    user.require_admin()?;
    if id == user.0.id {
        return Err(AppError::BadRequest("you cannot delete yourself".into()));
    }
    let res = sqlx::query("DELETE FROM users WHERE id = ? AND org_id = ? AND role != 'owner'")
        .bind(&id)
        .bind(user.org())
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    audit(&state, user.org(), Some(&user.0.id), "user.deleted", &id).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Serialize, sqlx::FromRow)]
struct AuditRow {
    id: i64,
    user_id: Option<String>,
    action: String,
    detail: String,
    created_at: String,
}

async fn list_audit(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Vec<AuditRow>>> {
    user.require_admin()?;
    let rows = sqlx::query_as::<_, AuditRow>(
        "SELECT id, user_id, action, detail, created_at FROM audit_log WHERE org_id = ? ORDER BY id DESC LIMIT 200",
    )
    .bind(user.org())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}


// ------------------------------------------------------------ password reset & invitations

fn bearer(h: &axum::http::HeaderMap) -> Option<String> {
    h.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(str::to_owned)
}

/// Base URL for links in emails: QIVO_PUBLIC_URL, else what the browser used.
pub fn public_url(h: &axum::http::HeaderMap) -> String {
    if let Some(u) = std::env::var("QIVO_PUBLIC_URL").ok().filter(|u| !u.trim().is_empty()) {
        return u.trim().trim_end_matches('/').to_string();
    }
    let host = h.get("x-forwarded-host").or_else(|| h.get(axum::http::header::HOST)).and_then(|v| v.to_str().ok()).unwrap_or("localhost:8080");
    let proto = h.get("x-forwarded-proto").and_then(|v| v.to_str().ok()).unwrap_or(if host.starts_with("localhost") || host.starts_with("127.") { "http" } else { "https" });
    format!("{proto}://{host}")
}

async fn issue_token(state: &AppState, user_id: &str, purpose: &str, valid: chrono::Duration) -> AppResult<String> {
    // One live token per user and purpose.
    sqlx::query("DELETE FROM password_tokens WHERE user_id = ? AND purpose = ?").bind(user_id).bind(purpose).execute(&state.db).await?;
    let token = random_token(48);
    sqlx::query("INSERT INTO password_tokens (token_hash, user_id, purpose, expires_at, created_at) VALUES (?,?,?,?,?)")
        .bind(sha256_hex(&token)).bind(user_id).bind(purpose)
        .bind((chrono::Utc::now() + valid).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)).bind(now())
        .execute(&state.db).await?;
    Ok(token)
}

#[derive(Deserialize)]
struct ForgotReq {
    email: String,
}

/// Always answers the same way, whether or not the account exists.
async fn forgot(State(state): State<AppState>, headers: axum::http::HeaderMap, Json(r): Json<ForgotReq>) -> AppResult<Json<Value>> {
    let email = r.email.trim().to_lowercase();
    let ip = client_ip(&headers);
    if !ratelimit::allow(&format!("forgot:ip:{ip}"), 10, HOUR) || !ratelimit::allow(&format!("forgot:acct:{email}"), 3, HOUR) {
        return Err(AppError::TooMany("too many reset requests — try again later".into()));
    }
    let user: Option<(String, String)> = sqlx::query_as("SELECT id, name FROM users WHERE email = ?").bind(&email).fetch_optional(&state.db).await?;
    if let Some((uid, name)) = user {
        let token = issue_token(&state, &uid, "reset", chrono::Duration::hours(1)).await?;
        let link = format!("{}/#/reset?token={token}", public_url(&headers));
        mail::send_bg(
            email.clone(),
            "Reset your Qivomedia password".into(),
            format!("Hello {name},\n\nUse this link to choose a new password (valid for 1 hour):\n{link}\n\nIf you didn't ask for this, you can ignore this email — your password stays the same.\n"),
            format!("<p>Hello {},</p><p>Someone (hopefully you) asked to reset your Qivomedia password.</p>{}<p style=\"font-size:13px;color:#4b5468\">The link is valid for 1 hour. If you didn't ask for this, ignore this email — your password stays the same.</p>",
                mail::esc(&name), mail::button(&link, "Choose a new password")),
        );
        tracing::info!("password reset requested for {email}");
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct TokenReq {
    token: String,
}

async fn valid_token(state: &AppState, token: &str) -> AppResult<(String, String, String, String)> {
    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT t.user_id, t.purpose, u.email, u.name FROM password_tokens t JOIN users u ON u.id = t.user_id
          WHERE t.token_hash = ? AND t.used_at IS NULL AND t.expires_at > ?",
    )
    .bind(sha256_hex(token)).bind(now())
    .fetch_optional(&state.db).await?;
    row.ok_or_else(|| AppError::BadRequest("this link has expired or was already used — ask for a new one".into()))
}

/// Lets the reset page greet the person ("Set your password, Amina").
async fn token_info(State(state): State<AppState>, headers: axum::http::HeaderMap, Json(r): Json<TokenReq>) -> AppResult<Json<Value>> {
    if !ratelimit::allow(&format!("token:ip:{}", client_ip(&headers)), 30, HOUR) {
        return Err(AppError::TooMany("too many attempts — try again later".into()));
    }
    let (_, purpose, email, name) = valid_token(&state, &r.token).await?;
    Ok(Json(json!({ "purpose": purpose, "email": email, "name": name })))
}

#[derive(Deserialize)]
struct ResetReq {
    token: String,
    password: String,
}

async fn reset_password(State(state): State<AppState>, headers: axum::http::HeaderMap, Json(r): Json<ResetReq>) -> AppResult<Json<Value>> {
    if !ratelimit::allow(&format!("reset:ip:{}", client_ip(&headers)), 20, HOUR) {
        return Err(AppError::TooMany("too many attempts — try again later".into()));
    }
    if r.password.len() < 8 {
        return Err(AppError::BadRequest("password must be at least 8 characters".into()));
    }
    let (uid, purpose, email, _) = valid_token(&state, &r.token).await?;
    let mut tx = state.db.begin().await?;
    // Mark used first: a token works exactly once even under concurrent requests.
    let used = sqlx::query("UPDATE password_tokens SET used_at = ? WHERE token_hash = ? AND used_at IS NULL")
        .bind(now()).bind(sha256_hex(&r.token)).execute(&mut *tx).await?;
    if used.rows_affected() != 1 {
        return Err(AppError::BadRequest("this link has expired or was already used — ask for a new one".into()));
    }
    sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?").bind(hash_password(&r.password)?).bind(&uid).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = ?").bind(&uid).execute(&mut *tx).await?;
    tx.commit().await?;
    ratelimit::reset(&format!("login:acct:{email}"));
    let org: String = sqlx::query_scalar("SELECT org_id FROM users WHERE id = ?").bind(&uid).fetch_one(&state.db).await?;
    audit(&state, &org, Some(&uid), if purpose == "invite" { "user.invite_accepted" } else { "user.password_reset" }, &email).await;
    let token = create_session(&state, &uid).await?;
    let user = user_from_token(&state, &token).await?;
    Ok(Json(json!({ "token": token, "user": user })))
}
