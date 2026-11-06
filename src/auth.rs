use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{FromRequestParts, Path, State},
    http::{header::SET_COOKIE, request::Parts, HeaderMap},
    response::{AppendHeaders, IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    mail, passwords,
    ratelimit::{self, client_ip, HOUR},
    session,
    state::AppState,
    util::{new_id, now, random_token, sha256_hex},
};

/// Failed logins allowed per account (and per IP) before a 15-minute lockout.
const MAX_FAILED: usize = 5;
const LOCKOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct User {
    pub id: String,
    pub org_id: String,
    pub email: String,
    pub name: String,
    pub role: String,
    pub created_at: String,
    pub platform_admin: bool,
    #[sqlx(default)]
    pub mfa_enabled: bool,
    /// The organization requires 2FA and this user hasn't set it up yet.
    #[sqlx(default)]
    pub mfa_setup_required: bool,
}

pub const USER_COLS: &str = "id, org_id, email, name, role, created_at, platform_admin, mfa_enabled";

/// Authenticated dashboard user, from the session cookie (or `Authorization: Bearer`).
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

type UserRow = (String, String, String, String, String, String, bool, bool, bool, bool, Option<String>);

pub async fn user_from_token(state: &AppState, token: &str) -> AppResult<User> {
    let idle_cutoff = (chrono::Utc::now() - chrono::Duration::hours(session::idle_hours()))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let row: Option<UserRow> = sqlx::query_as(
        "SELECT u.id, u.org_id, u.email, u.name, u.role, u.created_at, u.platform_admin, u.mfa_enabled,
                o.suspended, o.require_mfa, s.last_used_at
           FROM sessions s JOIN users u ON u.id = s.user_id JOIN organizations o ON o.id = u.org_id
          WHERE s.token_hash = ? AND s.expires_at > ? AND (s.last_used_at IS NULL OR s.last_used_at > ?)",
    )
    .bind(sha256_hex(token))
    .bind(now())
    .bind(&idle_cutoff)
    .fetch_optional(&state.db)
    .await?;
    let (id, org_id, email, name, role, created_at, platform_admin, mfa_enabled, suspended, require_mfa, last_used) =
        row.ok_or(AppError::Unauthorized)?;
    if suspended && !platform_admin {
        return Err(AppError::Forbidden("this account is suspended — contact your provider".into()));
    }
    // Sliding idle window; write at most once a minute.
    let stale = (chrono::Utc::now() - chrono::Duration::seconds(60)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    if last_used.map(|l| l < stale).unwrap_or(true) {
        let _ = sqlx::query("UPDATE sessions SET last_used_at = ? WHERE token_hash = ?").bind(now()).bind(sha256_hex(token)).execute(&state.db).await;
    }
    Ok(User { id, org_id, email, name, role, created_at, platform_admin, mfa_enabled, mfa_setup_required: require_mfa && !mfa_enabled })
}

/// Endpoints a user may reach while their organization still requires them to set up 2FA.
fn allowed_before_mfa_setup(path: &str) -> bool {
    let p = path.trim_start_matches("/api");
    p == "/auth/me" || p == "/auth/logout" || p.starts_with("/auth/mfa/") || p.starts_with("/auth/sessions")
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let token = session::token_from_headers(&parts.headers).ok_or(AppError::Unauthorized)?;
        let user = user_from_token(state, &token).await?;
        if user.mfa_setup_required && !allowed_before_mfa_setup(parts.uri.path()) {
            return Err(AppError::Forbidden("mfa_setup_required: your organization requires two-factor authentication".into()));
        }
        Ok(AuthUser(user))
    }
}

/// JSON body plus the session cookie.
pub fn with_session(cookie: axum::http::HeaderValue, body: Value) -> Response {
    (AppendHeaders([(SET_COOKIE, cookie)]), Json(body)).into_response()
}

pub fn hash_password(pw: &str) -> AppResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::Other(anyhow::anyhow!("hash: {e}")))
}

pub fn verify_password_pub(pw: &str, hash: &str) -> bool {
    verify_password(pw, hash)
}

fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

pub async fn audit(state: &AppState, org_id: &str, user_id: Option<&str>, action: &str, detail: &str) {
    audit_db(&state.db, org_id, user_id, action, detail).await
}

pub async fn audit_db(db: &sqlx::SqlitePool, org_id: &str, user_id: Option<&str>, action: &str, detail: &str) {
    let _ = sqlx::query("INSERT INTO audit_log (org_id, user_id, action, detail, created_at) VALUES (?,?,?,?,?)")
        .bind(org_id)
        .bind(user_id)
        .bind(action)
        .bind(detail)
        .bind(now())
        .execute(db)
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
        .merge(session::routes())
        .merge(crate::mfa::routes())
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

fn validate_email(email: &str) -> AppResult<()> {
    let ok = email.len() <= 200 && email.split_once('@').map(|(a, d)| !a.is_empty() && d.contains('.')).unwrap_or(false);
    if ok { Ok(()) } else { Err(AppError::BadRequest("valid email required".into())) }
}


async fn signup(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<SignupReq>) -> AppResult<Response> {
    if !ratelimit::allow(&format!("signup:{}", client_ip(&headers)), 10, HOUR) {
        return Err(AppError::TooMany("too many sign-ups from this network — try again later".into()));
    }
    let n = user_count(&state).await?;
    if n > 0 && !state.allow_signup {
        return Err(AppError::Forbidden("sign-up is disabled on this server".into()));
    }
    let email = req.email.trim().to_lowercase();
    validate_email(&email)?;
    passwords::check_new(&req.password, &[&email, &req.name, &req.org_name]).await?;
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
    let (token, cookie) = session::create(&state, &user_id, &headers, false).await?;
    audit(&state, &org_id, Some(&user_id), "org.created", req.org_name.trim()).await;
    let user = user_from_token(&state, &token).await?;
    Ok(with_session(cookie, json!({ "user": user })))
}

#[derive(Deserialize)]
struct LoginReq {
    email: String,
    password: String,
}

async fn login(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<LoginReq>) -> AppResult<Response> {
    let email = req.email.trim().to_lowercase();
    let ip = client_ip(&headers);
    let (acct_key, ip_key) = (format!("login:acct:{email}"), format!("login:ip:{ip}"));
    // Lock an account after repeated failures, and an IP that tries many accounts.
    if ratelimit::exceeded(&acct_key, MAX_FAILED, LOCKOUT) || ratelimit::exceeded(&ip_key, MAX_FAILED * 6, LOCKOUT) {
        return Err(AppError::TooMany("too many failed attempts — wait 15 minutes or reset your password".into()));
    }
    let row: Option<(String, String, String, bool)> = sqlx::query_as("SELECT id, password_hash, org_id, mfa_enabled FROM users WHERE email = ?")
        .bind(&email)
        .fetch_optional(&state.db)
        .await?;
    let Some((uid, hash, org_id, mfa_enabled)) = row else {
        // Spend comparable time to avoid user enumeration via timing.
        let _ = hash_password(&req.password);
        ratelimit::hit(&acct_key);
        ratelimit::hit(&ip_key);
        return Err(AppError::Unauthorized);
    };
    if !verify_password(&req.password, &hash) {
        ratelimit::hit(&acct_key);
        ratelimit::hit(&ip_key);
        audit(&state, &org_id, Some(&uid), "auth.login_failed", &who(&headers)).await;
        return Err(AppError::Unauthorized);
    }
    ratelimit::reset(&acct_key);
    if mfa_enabled {
        // Second step required: hand back a short-lived ticket, not a session.
        let ticket = crate::mfa::issue_ticket(&uid);
        return Ok(Json(json!({ "mfa_required": true, "ticket": ticket })).into_response());
    }
    sqlx::query("DELETE FROM sessions WHERE expires_at < ?").bind(now()).execute(&state.db).await?;
    let (token, cookie) = session::create(&state, &uid, &headers, false).await?;
    audit(&state, &org_id, Some(&uid), "auth.login", &who(&headers)).await;
    let user = user_from_token(&state, &token).await?;
    Ok(with_session(cookie, json!({ "user": user })))
}

/// "ip · user agent" for security audit entries.
pub fn who(h: &HeaderMap) -> String {
    let ua: String = h.get(axum::http::header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").chars().take(120).collect();
    format!("{} · {ua}", client_ip(h))
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> AppResult<Response> {
    if let Some(t) = session::token_from_headers(&headers) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?").bind(sha256_hex(&t)).execute(&state.db).await?;
    }
    let mut res = Json(json!({ "ok": true })).into_response();
    for c in session::clear_cookies(&headers) {
        res.headers_mut().append(SET_COOKIE, c);
    }
    Ok(res)
}

async fn me(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    let org: (String, String, Option<i64>, bool, bool) =
        sqlx::query_as("SELECT name, plan, max_screens, ai_enabled, require_mfa FROM organizations WHERE id = ?")
            .bind(user.org())
            .fetch_one(&state.db)
            .await?;
    Ok(Json(json!({
        "user": user.0,
        "organization": { "id": user.org(), "name": org.0, "plan": org.1, "max_screens": org.2, "require_mfa": org.4 },
        "ai": { "available": crate::ai::configured() && org.3 },
    })))
}

async fn list_users(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Vec<User>>> {
    let users = sqlx::query_as::<_, User>(
        &format!("SELECT {USER_COLS} FROM users WHERE org_id = ? ORDER BY created_at"),
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

async fn create_user(State(state): State<AppState>, headers: HeaderMap, user: AuthUser, Json(req): Json<CreateUserReq>) -> AppResult<Json<User>> {
    user.require_admin()?;
    let email = req.email.trim().to_lowercase();
    let password = if req.invite { random_token(32) } else { req.password.clone() };
    validate_email(&email)?;
    if !req.invite {
        passwords::check_new(&password, &[&email, &req.name]).await?;
    }
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
    let u = sqlx::query_as::<_, User>(&format!("SELECT {USER_COLS} FROM users WHERE id = ?"))
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
    /// Admin action for a member who lost their authenticator.
    #[serde(default)]
    reset_mfa: bool,
}

async fn update_user(
    State(state): State<AppState>,
    headers: HeaderMap,
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
    if req.reset_mfa {
        user.require_admin()?;
        if is_self {
            return Err(AppError::BadRequest("use Security settings to change your own two-factor authentication".into()));
        }
        let (email, name, _) = crate::mfa::reset_for_user(&state.db, &id).await?;
        audit(&state, user.org(), Some(&user.0.id), "auth.mfa_reset_by_admin", &email).await;
        crate::mfa::notify_reset(&email, &name, &format!("{}, an administrator of your organization", user.0.name)).await;
    }
    if let Some(pw) = &req.password {
        let (email, name): (String, String) = sqlx::query_as("SELECT email, name FROM users WHERE id = ?").bind(&id).fetch_one(&state.db).await?;
        passwords::check_new(pw, &[&email, &name]).await?;
        sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(hash_password(pw)?)
            .bind(&id)
            .execute(&state.db)
            .await?;
        // Sign out every other session of that user (keep the caller's own session).
        let current = session::token_from_headers(&headers).map(|t| sha256_hex(&t)).unwrap_or_default();
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


/// Base URL for links in emails: QIVO_PUBLIC_URL, else what the browser used.
pub fn public_url(h: &HeaderMap) -> String {
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
async fn forgot(State(state): State<AppState>, headers: HeaderMap, Json(r): Json<ForgotReq>) -> AppResult<Json<Value>> {
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
async fn token_info(State(state): State<AppState>, headers: HeaderMap, Json(r): Json<TokenReq>) -> AppResult<Json<Value>> {
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

async fn reset_password(State(state): State<AppState>, headers: HeaderMap, Json(r): Json<ResetReq>) -> AppResult<Response> {
    if !ratelimit::allow(&format!("reset:ip:{}", client_ip(&headers)), 20, HOUR) {
        return Err(AppError::TooMany("too many attempts — try again later".into()));
    }
    let (uid, purpose, email, name) = valid_token(&state, &r.token).await?;
    passwords::check_new(&r.password, &[&email, &name]).await?;
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
    // With 2FA on, a reset link alone must not grant a session: sign in normally (password + code).
    let mfa_enabled: bool = sqlx::query_scalar("SELECT mfa_enabled FROM users WHERE id = ?").bind(&uid).fetch_one(&state.db).await?;
    if mfa_enabled {
        return Ok(Json(json!({ "ok": true, "signin_required": true })).into_response());
    }
    let (token, cookie) = session::create(&state, &uid, &headers, false).await?;
    let user = user_from_token(&state, &token).await?;
    Ok(with_session(cookie, json!({ "user": user })))
}
