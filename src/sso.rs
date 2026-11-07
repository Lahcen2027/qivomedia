//! Single sign-on with OpenID Connect: Microsoft Entra ID (Microsoft 365), Google Workspace,
//! or any standard provider (Keycloak, Okta, ADFS 2016+, Shibboleth OIDC…).
//!
//! Flow: Authorization Code + PKCE (S256), `state` bound to the browser by a cookie, `nonce`
//! inside the ID token. The ID token is checked against the provider's published keys (JWKS),
//! issuer, audience, expiry and nonce. Identities are linked to users by the provider's
//! stable subject id; on first sign-in an existing user of the same organization is matched
//! by email, or a user is created if the organization allows it.
//!
//! Which email we trust per provider:
//! * Microsoft: `preferred_username` (the UPN). UPN suffixes must be domains the tenant
//!   has verified, whereas the `email` claim can be edited and is not verified ("nOAuth").
//!   Guest accounts (`…#EXT#@tenant.onmicrosoft.com`) therefore fail the domain check.
//! * Google: `email` with `email_verified`, and the Workspace domain claim `hd` must be
//!   one of the allowed domains, so personal Gmail accounts are refused.
//! * Other providers: `email`, refused if the provider says it is not verified.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, put},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64URL, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    auth::{audit, public_url, who, AuthUser},
    ratelimit::client_ip,
    error::{AppError, AppResult},
    ratelimit, session,
    state::AppState,
    util::{now, random_token, sha256_hex},
};

const STATE_TTL_MIN: i64 = 10;
const CACHE_TTL: Duration = Duration::from_secs(3600);

pub fn routes() -> Router<AppState> {
    Router::new()
        // public (sign-in)
        .route("/auth/sso/discover", get(discover))
        .route("/auth/sso/{id}/start", get(start))
        .route("/auth/sso/callback", get(callback))
        // admin (settings)
        .route("/sso/providers", get(list).post(create))
        .route("/sso/providers/{id}", put(update).delete(remove))
        .route("/sso/policy", put(set_policy))
}

// ---------------------------------------------------------------- provider metadata

#[derive(Clone, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

static DISCOVERY: Mutex<Option<HashMap<String, (Instant, Discovery)>>> = Mutex::new(None);
static JWKS: Mutex<Option<HashMap<String, (Instant, jsonwebtoken::jwk::JwkSet)>>> = Mutex::new(None);

fn http() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(10)).build().expect("http client")
}

/// Providers must use HTTPS. Plain HTTP is accepted only on this machine (tests, local IdPs).
fn check_url(u: &str) -> AppResult<()> {
    let url = reqwest::Url::parse(u).map_err(|_| AppError::BadRequest(format!("not a valid address: {u}")))?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if url.scheme() == "https" || (url.scheme() == "http" && local) {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!("the identity provider must use https: {u}")))
    }
}

async fn discovery(issuer: &str) -> AppResult<Discovery> {
    if let Some((t, d)) = DISCOVERY.lock().unwrap().get_or_insert_with(HashMap::new).get(issuer) {
        if t.elapsed() < CACHE_TTL {
            return Ok(d.clone());
        }
    }
    let url = format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'));
    check_url(&url)?;
    let d: Discovery = http()
        .get(&url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| AppError::BadRequest(format!("could not reach the identity provider ({url}): {e}")))?
        .json()
        .await
        .map_err(|_| AppError::BadRequest(format!("{url} is not an OpenID Connect discovery document")))?;
    for u in [&d.authorization_endpoint, &d.token_endpoint, &d.jwks_uri] {
        check_url(u)?;
    }
    DISCOVERY.lock().unwrap().get_or_insert_with(HashMap::new).insert(issuer.to_owned(), (Instant::now(), d.clone()));
    Ok(d)
}

async fn jwks(uri: &str, force: bool) -> AppResult<jsonwebtoken::jwk::JwkSet> {
    if !force {
        if let Some((t, k)) = JWKS.lock().unwrap().get_or_insert_with(HashMap::new).get(uri) {
            if t.elapsed() < CACHE_TTL {
                return Ok(k.clone());
            }
        }
    }
    let set: jsonwebtoken::jwk::JwkSet = http()
        .get(uri)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| AppError::Other(anyhow::anyhow!("JWKS fetch failed: {e}")))?
        .json()
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("JWKS parse failed: {e}")))?;
    JWKS.lock().unwrap().get_or_insert_with(HashMap::new).insert(uri.to_owned(), (Instant::now(), set.clone()));
    Ok(set)
}

// ---------------------------------------------------------------- provider records

#[derive(sqlx::FromRow, Clone)]
struct Provider {
    id: String,
    org_id: String,
    kind: String,
    name: String,
    issuer: String,
    client_id: String,
    client_secret_enc: String,
    domains: String,
    auto_create: bool,
    default_role: String,
    group_roles: String,
    enabled: bool,
}

const PROVIDER_COLS: &str = "id, org_id, kind, name, issuer, client_id, client_secret_enc, domains, auto_create, default_role, group_roles, enabled";

impl Provider {
    fn domain_list(&self) -> Vec<String> {
        split_domains(&self.domains)
    }
    fn public(&self, redirect_uri: &str) -> Value {
        json!({
            "id": self.id, "kind": self.kind, "name": self.name, "issuer": self.issuer, "client_id": self.client_id,
            "has_secret": !self.client_secret_enc.is_empty(), "domains": self.domain_list(), "auto_create": self.auto_create,
            "default_role": self.default_role, "group_roles": serde_json::from_str::<Value>(&self.group_roles).unwrap_or(json!([])),
            "enabled": self.enabled, "redirect_uri": redirect_uri,
        })
    }
}

fn split_domains(s: &str) -> Vec<String> {
    s.split([',', ' ', ';', '\n'])
        .map(|d| d.trim().trim_start_matches('@').to_lowercase())
        .filter(|d| !d.is_empty())
        .collect()
}

async fn provider(state: &AppState, id: &str) -> AppResult<Provider> {
    sqlx::query_as::<_, Provider>(&format!("SELECT {PROVIDER_COLS} FROM sso_providers WHERE id = ?"))
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)
}

fn redirect_uri(h: &HeaderMap) -> String {
    format!("{}/api/auth/sso/callback", public_url(h))
}

// ---------------------------------------------------------------- sign-in

#[derive(Deserialize)]
struct DiscoverQ {
    email: String,
}

/// Which provider handles this email address (by domain). Answers the same shape whether or
/// not one exists, and reveals nothing about which users have accounts.
async fn discover(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<DiscoverQ>) -> AppResult<Json<Value>> {
    if !ratelimit::allow(&format!("sso-discover:{}", client_ip(&headers)), 60, ratelimit::MINUTE) {
        return Err(AppError::TooMany("too many requests".into()));
    }
    let domain = q.email.trim().rsplit('@').next().unwrap_or("").to_lowercase();
    if domain.is_empty() || !q.email.contains('@') {
        return Ok(Json(json!({ "provider": null })));
    }
    let rows: Vec<(String, String, String, String)> =
        sqlx::query_as("SELECT id, name, kind, domains FROM sso_providers WHERE enabled = 1").fetch_all(&state.db).await?;
    let hit = rows.into_iter().find(|(_, _, _, d)| split_domains(d).contains(&domain));
    Ok(Json(json!({ "provider": hit.map(|(id, name, kind, _)| json!({ "id": id, "name": name, "kind": kind,
        "start_url": format!("/api/auth/sso/{id}/start") })) })))
}

fn state_cookie_name(h: &HeaderMap) -> &'static str {
    if session::is_https(h) { "__Host-qivo_sso" } else { "qivo_sso" }
}

/// Redirect to the identity provider.
async fn start(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> AppResult<Response> {
    if !ratelimit::allow(&format!("sso-start:{}", client_ip(&headers)), 30, ratelimit::MINUTE) {
        return Err(AppError::TooMany("too many sign-in attempts".into()));
    }
    let p = provider(&state, &id).await?;
    if !p.enabled {
        return Ok(back_to_login("disabled"));
    }
    let d = match discovery(&p.issuer).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("SSO {}: {e}", p.name);
            return Ok(back_to_login("provider_unreachable"));
        }
    };
    let st = random_token(32);
    let nonce = random_token(32);
    let verifier = random_token(48);
    let challenge = B64URL.encode(Sha256::digest(verifier.as_bytes()));
    let redirect = redirect_uri(&headers);
    sqlx::query("DELETE FROM sso_states WHERE created_at < ?")
        .bind((chrono::Utc::now() - chrono::Duration::minutes(STATE_TTL_MIN)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .execute(&state.db)
        .await?;
    sqlx::query("INSERT INTO sso_states (state_hash, provider_id, nonce, verifier, redirect_uri, created_at) VALUES (?,?,?,?,?,?)")
        .bind(sha256_hex(&st))
        .bind(&p.id)
        .bind(&nonce)
        .bind(&verifier)
        .bind(&redirect)
        .bind(now())
        .execute(&state.db)
        .await?;
    let mut url = reqwest::Url::parse(&d.authorization_endpoint).map_err(|e| AppError::Other(e.into()))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", &p.client_id)
            .append_pair("redirect_uri", &redirect)
            .append_pair("scope", "openid email profile")
            .append_pair("state", &st)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        match p.kind.as_str() {
            "microsoft" => {
                q.append_pair("response_mode", "query").append_pair("prompt", "select_account");
            }
            "google" => {
                // Shows only accounts of the university's Workspace domain (the check itself is server-side).
                if let [only] = p.domain_list().as_slice() {
                    q.append_pair("hd", only);
                }
                q.append_pair("prompt", "select_account");
            }
            _ => {}
        }
    }
    // SameSite=Lax: the cookie must come back on the provider's top-level redirect.
    let secure = if session::is_https(&headers) { "; Secure" } else { "" };
    let cookie = format!("{}={st}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}", state_cookie_name(&headers), STATE_TTL_MIN * 60);
    Ok((StatusCode::SEE_OTHER, [(header::LOCATION, url.to_string()), (header::SET_COOKIE, cookie)]).into_response())
}

#[derive(Deserialize)]
struct CallbackQ {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct TokenResp {
    id_token: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Claims {
    sub: String,
    email: Option<String>,
    email_verified: Option<Value>,
    preferred_username: Option<String>,
    name: Option<String>,
    hd: Option<String>,
    nonce: Option<String>,
    groups: Option<Vec<String>>,
    #[serde(rename = "_claim_names")]
    claim_names: Option<Value>,
}

/// Where the browser goes back to after the provider.
async fn callback(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<CallbackQ>) -> Response {
    match finish(&state, &headers, q).await {
        Ok(resp) => resp,
        Err(code) => back_to_login(code),
    }
}

/// Sign-in failed: back to the login screen with a reason it can explain.
fn back_to_login(code: &str) -> Response {
    let html = format!(
        "<!doctype html><meta charset=utf-8><meta http-equiv=refresh content=\"0;url=/#/login?sso_error={code}\"><title>Qivomedia</title>"
    );
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], html).into_response()
}

async fn finish(state: &AppState, headers: &HeaderMap, q: CallbackQ) -> Result<Response, &'static str> {
    if let Some(e) = q.error {
        tracing::info!("SSO: provider returned {e}: {}", q.error_description.unwrap_or_default());
        return Err(if e == "access_denied" { "cancelled" } else { "provider_error" });
    }
    let (Some(code), Some(st)) = (q.code, q.state) else { return Err("invalid") };
    // The state must be the one this browser started with (cookie) and still pending (database).
    let cookie_state = cookie_value(headers, state_cookie_name(headers));
    if cookie_state.as_deref() != Some(st.as_str()) {
        return Err("session_mismatch");
    }
    let row: Option<(String, String, String, String, String)> =
        sqlx::query_as("SELECT provider_id, nonce, verifier, redirect_uri, created_at FROM sso_states WHERE state_hash = ?")
            .bind(sha256_hex(&st))
            .fetch_optional(&state.db)
            .await
            .map_err(|_| "server")?;
    let Some((provider_id, nonce, verifier, redirect, created)) = row else { return Err("expired") };
    let _ = sqlx::query("DELETE FROM sso_states WHERE state_hash = ?").bind(sha256_hex(&st)).execute(&state.db).await;
    let age = chrono::DateTime::parse_from_rfc3339(&created).map(|t| chrono::Utc::now() - t.with_timezone(&chrono::Utc)).unwrap_or(chrono::Duration::days(1));
    if age > chrono::Duration::minutes(STATE_TTL_MIN) {
        return Err("expired");
    }
    let p = provider(state, &provider_id).await.map_err(|_| "expired")?;
    if !p.enabled {
        return Err("disabled");
    }
    let claims = exchange_and_verify(&p, &code, &verifier, &redirect, &nonce).await.map_err(|e| {
        tracing::warn!("SSO {}: {e}", p.name);
        "token"
    })?;
    let email = trusted_email(&p, &claims)?;
    let user_id = resolve_user(state, &p, &claims, &email).await?;

    let (token, cookie) = session::create(state, &user_id, headers, false).await.map_err(|_| "server")?;
    let _ = sqlx::query("UPDATE sessions SET sso = 1 WHERE token_hash = ?").bind(sha256_hex(&token)).execute(&state.db).await;
    audit(state, &p.org_id, Some(&user_id), "auth.sso_login", &format!("{} · {} · {}", p.name, email, who(headers))).await;

    let clear = format!("{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}", state_cookie_name(headers), if session::is_https(headers) { "; Secure" } else { "" });
    // A same-site page hop (instead of a redirect), so the strict session cookie is sent on
    // the very first dashboard request.
    let html = "<!doctype html><meta charset=utf-8><meta http-equiv=refresh content=\"0;url=/#/\"><title>Qivomedia</title>";
    let mut resp = ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], html).into_response();
    resp.headers_mut().append(header::SET_COOKIE, cookie);
    if let Ok(v) = HeaderValue::from_str(&clear) {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
    Ok(resp)
}

fn cookie_value(h: &HeaderMap, name: &str) -> Option<String> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

async fn exchange_and_verify(p: &Provider, code: &str, verifier: &str, redirect: &str, nonce: &str) -> anyhow::Result<Claims> {
    let d = discovery(&p.issuer).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let secret = crate::crypto::open(&p.client_secret_enc)?;
    let resp = http()
        .post(&d.token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect),
            ("client_id", &p.client_id),
            ("client_secret", &secret),
            ("code_verifier", verifier),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body: String = resp.text().await.unwrap_or_default().chars().take(300).collect();
        anyhow::bail!("token endpoint answered {status}: {body}");
    }
    let tok: TokenResp = resp.json().await?;
    let claims = verify_id_token(&tok.id_token, &d, &p.client_id).await?;
    anyhow::ensure!(claims.nonce.as_deref() == Some(nonce), "nonce mismatch");
    anyhow::ensure!(!claims.sub.is_empty(), "no subject");
    Ok(claims)
}

async fn verify_id_token(id_token: &str, d: &Discovery, client_id: &str) -> anyhow::Result<Claims> {
    use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
    let header = decode_header(id_token)?;
    anyhow::ensure!(
        matches!(header.alg, Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 | Algorithm::ES256 | Algorithm::ES384 | Algorithm::PS256),
        "unsupported signing algorithm {:?}",
        header.alg
    );
    let kid = header.kid.clone().unwrap_or_default();
    let find = |set: &jsonwebtoken::jwk::JwkSet| -> Option<jsonwebtoken::jwk::Jwk> {
        if kid.is_empty() { set.keys.first().cloned() } else { set.find(&kid).cloned() }
    };
    // Keys rotate: on an unknown key id, refresh the key set once.
    let jwk = match find(&jwks(&d.jwks_uri, false).await.map_err(|e| anyhow::anyhow!("{e}"))?) {
        Some(k) => k,
        None => find(&jwks(&d.jwks_uri, true).await.map_err(|e| anyhow::anyhow!("{e}"))?).ok_or_else(|| anyhow::anyhow!("unknown signing key {kid}"))?,
    };
    let key = DecodingKey::from_jwk(&jwk)?;
    let mut v = Validation::new(header.alg);
    v.set_issuer(&[&d.issuer]);
    v.set_audience(&[client_id]);
    v.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    v.leeway = 120;
    Ok(decode::<Claims>(id_token, &key, &v)?.claims)
}

fn domain_of(email: &str) -> String {
    email.rsplit_once('@').map(|(_, d)| d.to_lowercase()).unwrap_or_default()
}

/// The email address we are willing to trust for this provider (see the module docs).
fn trusted_email(p: &Provider, c: &Claims) -> Result<String, &'static str> {
    let verified_false = matches!(&c.email_verified, Some(Value::Bool(false))) || matches!(&c.email_verified, Some(Value::String(s)) if s == "false");
    let email = match p.kind.as_str() {
        "microsoft" => c.preferred_username.clone().or_else(|| c.email.clone()),
        "google" => {
            if !matches!(&c.email_verified, Some(Value::Bool(true))) && !matches!(&c.email_verified, Some(Value::String(s)) if s == "true") {
                return Err("email_unverified");
            }
            let hd = c.hd.as_deref().unwrap_or("").to_lowercase();
            if hd.is_empty() || !p.domain_list().contains(&hd) {
                return Err("domain");
            }
            c.email.clone()
        }
        _ => {
            if verified_false {
                return Err("email_unverified");
            }
            c.email.clone().or_else(|| c.preferred_username.clone())
        }
    };
    let email = email.map(|e| e.trim().to_lowercase()).filter(|e| e.contains('@') && !e.contains('#')).ok_or("no_email")?;
    let domains = p.domain_list();
    if !domains.is_empty() && !domains.contains(&domain_of(&email)) {
        return Err("domain");
    }
    Ok(email)
}

const RANK: [&str; 3] = ["viewer", "editor", "admin"];

/// Role from the provider's groups (highest match wins), else the default role.
fn role_from_groups(p: &Provider, c: &Claims) -> Option<String> {
    let rules: Vec<Value> = serde_json::from_str(&p.group_roles).unwrap_or_default();
    if rules.is_empty() {
        return None;
    }
    if c.groups.is_none() && c.claim_names.is_some() {
        // Microsoft sends a link instead of the list when someone is in too many groups (>200).
        tracing::warn!("SSO {}: group list too long to be included in the token; using the default role", p.name);
    }
    let groups: Vec<String> = c.groups.clone().unwrap_or_default().into_iter().map(|g| g.to_lowercase()).collect();
    let best = rules
        .iter()
        .filter(|r| r["group"].as_str().map(|g| groups.contains(&g.trim().to_lowercase())).unwrap_or(false))
        .filter_map(|r| r["role"].as_str())
        .filter_map(|r| RANK.iter().position(|x| *x == r))
        .max();
    Some(best.map(|i| RANK[i].to_owned()).unwrap_or_else(|| p.default_role.clone()))
}

async fn resolve_user(state: &AppState, p: &Provider, c: &Claims, email: &str) -> Result<String, &'static str> {
    let db = &state.db;
    let suspended: bool = sqlx::query_scalar("SELECT suspended FROM organizations WHERE id = ?").bind(&p.org_id).fetch_one(db).await.map_err(|_| "server")?;
    if suspended {
        return Err("suspended");
    }
    let linked: Option<String> = sqlx::query_scalar("SELECT user_id FROM user_identities WHERE provider_id = ? AND subject = ?")
        .bind(&p.id)
        .bind(&c.sub)
        .fetch_optional(db)
        .await
        .map_err(|_| "server")?;
    let group_role = role_from_groups(p, c);
    let user_id = if let Some(uid) = linked {
        uid
    } else {
        let existing: Option<(String, String)> = sqlx::query_as("SELECT id, org_id FROM users WHERE email = ?").bind(email).fetch_optional(db).await.map_err(|_| "server")?;
        let uid = match existing {
            Some((uid, org)) if org == p.org_id => uid,
            Some(_) => return Err("other_org"),
            None if p.auto_create => {
                let uid = uuid::Uuid::new_v4().to_string();
                let role = group_role.clone().unwrap_or_else(|| p.default_role.clone());
                let name = c.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| email.split('@').next().unwrap_or(email).to_owned());
                // "!sso" is not a valid password hash: this account has no password until someone sets one.
                sqlx::query("INSERT INTO users (id, org_id, email, name, password_hash, role, created_at) VALUES (?,?,?,?,?,?,?)")
                    .bind(&uid)
                    .bind(&p.org_id)
                    .bind(email)
                    .bind(name.chars().take(100).collect::<String>())
                    .bind("!sso")
                    .bind(&role)
                    .bind(now())
                    .execute(db)
                    .await
                    .map_err(|_| "server")?;
                audit(state, &p.org_id, Some(&uid), "auth.sso_user_created", &format!("{email} as {role} via {}", p.name)).await;
                uid
            }
            None => return Err("not_invited"),
        };
        sqlx::query("INSERT INTO user_identities (provider_id, subject, user_id, email, created_at) VALUES (?,?,?,?,?)")
            .bind(&p.id)
            .bind(&c.sub)
            .bind(&uid)
            .bind(email)
            .bind(now())
            .execute(db)
            .await
            .map_err(|_| "server")?;
        uid
    };
    // Keep the role in step with the university's groups (never touches the owner).
    if let Some(role) = group_role {
        let current: Option<String> = sqlx::query_scalar("SELECT role FROM users WHERE id = ?").bind(&user_id).fetch_optional(db).await.map_err(|_| "server")?;
        if let Some(cur) = current {
            if cur != "owner" && cur != role {
                let _ = sqlx::query("UPDATE users SET role = ? WHERE id = ?").bind(&role).bind(&user_id).execute(db).await;
                audit(state, &p.org_id, Some(&user_id), "auth.sso_role_changed", &format!("{email}: {cur} → {role} (groups)")).await;
            }
        }
    }
    let _ = sqlx::query("UPDATE user_identities SET last_login_at = ?, email = ? WHERE provider_id = ? AND subject = ?")
        .bind(now())
        .bind(email)
        .bind(&p.id)
        .bind(&c.sub)
        .execute(db)
        .await;
    Ok(user_id)
}

// ---------------------------------------------------------------- settings (admins)

async fn list(State(state): State<AppState>, headers: HeaderMap, user: AuthUser) -> AppResult<Json<Value>> {
    user.require_admin()?;
    let rows = sqlx::query_as::<_, Provider>(&format!("SELECT {PROVIDER_COLS} FROM sso_providers WHERE org_id = ? ORDER BY created_at"))
        .bind(user.org())
        .fetch_all(&state.db)
        .await?;
    let required: bool = sqlx::query_scalar("SELECT sso_required FROM organizations WHERE id = ?").bind(user.org()).fetch_one(&state.db).await?;
    let ru = redirect_uri(&headers);
    Ok(Json(json!({
        "providers": rows.iter().map(|p| p.public(&ru)).collect::<Vec<_>>(),
        "sso_required": required,
        "redirect_uri": ru,
    })))
}

#[derive(Deserialize)]
struct ProviderReq {
    kind: Option<String>,
    name: Option<String>,
    /// Microsoft: directory (tenant) id or primary domain. Other OIDC: the issuer URL. Google: ignored.
    tenant: Option<String>,
    issuer: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    domains: Option<String>,
    auto_create: Option<bool>,
    default_role: Option<String>,
    group_roles: Option<Value>,
    enabled: Option<bool>,
}

fn valid_role(r: &str) -> bool {
    RANK.contains(&r)
}

/// Check the settings and find the provider's canonical issuer.
async fn resolve_issuer(kind: &str, tenant: Option<&str>, issuer: Option<&str>) -> AppResult<String> {
    let candidate = match kind {
        "microsoft" => {
            let t = tenant.map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| AppError::BadRequest("enter the directory (tenant) ID".into()))?;
            if ["common", "organizations", "consumers"].contains(&t) || !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
                return Err(AppError::BadRequest("use your university's own directory (tenant) ID, not a shared one".into()));
            }
            format!("https://login.microsoftonline.com/{t}/v2.0")
        }
        "google" => "https://accounts.google.com".to_owned(),
        "oidc" => issuer.map(|s| s.trim().trim_end_matches('/').to_owned()).filter(|s| !s.is_empty()).ok_or_else(|| AppError::BadRequest("enter the issuer URL".into()))?,
        _ => return Err(AppError::BadRequest("kind must be microsoft, google or oidc".into())),
    };
    check_url(&candidate)?;
    let d = discovery(&candidate).await?;
    // Microsoft answers for a domain name with its tenant GUID: keep that canonical issuer.
    if kind == "oidc" && d.issuer.trim_end_matches('/') != candidate {
        return Err(AppError::BadRequest(format!("the provider says its issuer is {}, not {candidate}", d.issuer)));
    }
    Ok(d.issuer)
}

async fn check_domains_free(state: &AppState, org: &str, domains: &[String], except: Option<&str>) -> AppResult<()> {
    let rows: Vec<(String, String, String)> = sqlx::query_as("SELECT id, org_id, domains FROM sso_providers").fetch_all(&state.db).await?;
    for (id, o, d) in rows {
        if Some(id.as_str()) == except {
            continue;
        }
        if let Some(clash) = split_domains(&d).into_iter().find(|x| domains.contains(x)) {
            return Err(AppError::Conflict(if o == org {
                format!("{clash} is already used by another sign-in provider of this organization")
            } else {
                format!("{clash} is already claimed by another organization — contact support")
            }));
        }
    }
    Ok(())
}

fn normalize_group_roles(v: Option<&Value>) -> AppResult<String> {
    let arr = v.and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut out = vec![];
    for r in arr {
        let g = r["group"].as_str().unwrap_or("").trim();
        let role = r["role"].as_str().unwrap_or("");
        if g.is_empty() {
            continue;
        }
        if !valid_role(role) {
            return Err(AppError::BadRequest(format!("role for group {g} must be admin, editor or viewer")));
        }
        out.push(json!({ "group": g, "role": role }));
    }
    Ok(Value::Array(out).to_string())
}

async fn create(State(state): State<AppState>, headers: HeaderMap, user: AuthUser, Json(r): Json<ProviderReq>) -> AppResult<Json<Value>> {
    user.require_admin()?;
    let kind = r.kind.clone().unwrap_or_default();
    let name = r.name.clone().map(|n| n.trim().to_owned()).filter(|n| !n.is_empty()).ok_or_else(|| AppError::BadRequest("enter a name for the sign-in button".into()))?;
    let client_id = r.client_id.clone().map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()).ok_or_else(|| AppError::BadRequest("enter the client (application) ID".into()))?;
    let secret = r.client_secret.clone().map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()).ok_or_else(|| AppError::BadRequest("enter the client secret".into()))?;
    let domains = split_domains(r.domains.as_deref().unwrap_or(""));
    if kind == "google" && domains.is_empty() {
        return Err(AppError::BadRequest("Google needs your Workspace domain (e.g. univ.ma), otherwise any Gmail account could sign in".into()));
    }
    let default_role = r.default_role.clone().unwrap_or_else(|| "viewer".into());
    if !valid_role(&default_role) {
        return Err(AppError::BadRequest("default role must be admin, editor or viewer".into()));
    }
    check_domains_free(&state, user.org(), &domains, None).await?;
    let issuer = resolve_issuer(&kind, r.tenant.as_deref(), r.issuer.as_deref()).await?;
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO sso_providers (id, org_id, kind, name, issuer, client_id, client_secret_enc, domains, auto_create, default_role, group_roles, enabled, created_at)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(user.org())
    .bind(&kind)
    .bind(&name)
    .bind(&issuer)
    .bind(&client_id)
    .bind(crate::crypto::seal(&secret)?)
    .bind(domains.join(","))
    .bind(r.auto_create.unwrap_or(true))
    .bind(&default_role)
    .bind(normalize_group_roles(r.group_roles.as_ref())?)
    .bind(r.enabled.unwrap_or(true))
    .bind(now())
    .execute(&state.db)
    .await?;
    audit(&state, user.org(), Some(&user.0.id), "sso.provider_added", &format!("{name} ({kind})")).await;
    Ok(Json(provider(&state, &id).await?.public(&redirect_uri(&headers))))
}

async fn update(State(state): State<AppState>, headers: HeaderMap, user: AuthUser, Path(id): Path<String>, Json(r): Json<ProviderReq>) -> AppResult<Json<Value>> {
    user.require_admin()?;
    let mut p = provider(&state, &id).await?;
    if p.org_id != user.org() {
        return Err(AppError::NotFound);
    }
    if let Some(n) = r.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        p.name = n.to_owned();
    }
    if let Some(c) = r.client_id.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        p.client_id = c.to_owned();
    }
    if let Some(s) = r.client_secret.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        p.client_secret_enc = crate::crypto::seal(s)?;
    }
    if r.tenant.as_deref().is_some_and(|t| !t.trim().is_empty()) || r.issuer.as_deref().is_some_and(|t| !t.trim().is_empty()) {
        p.issuer = resolve_issuer(&p.kind, r.tenant.as_deref(), r.issuer.as_deref()).await?;
    }
    if let Some(d) = &r.domains {
        let list = split_domains(d);
        if p.kind == "google" && list.is_empty() {
            return Err(AppError::BadRequest("Google needs at least one Workspace domain".into()));
        }
        check_domains_free(&state, user.org(), &list, Some(&id)).await?;
        p.domains = list.join(",");
    }
    if let Some(a) = r.auto_create {
        p.auto_create = a;
    }
    if let Some(dr) = &r.default_role {
        if !valid_role(dr) {
            return Err(AppError::BadRequest("default role must be admin, editor or viewer".into()));
        }
        p.default_role = dr.clone();
    }
    if r.group_roles.is_some() {
        p.group_roles = normalize_group_roles(r.group_roles.as_ref())?;
    }
    if let Some(e) = r.enabled {
        p.enabled = e;
    }
    sqlx::query("UPDATE sso_providers SET name=?, client_id=?, client_secret_enc=?, issuer=?, domains=?, auto_create=?, default_role=?, group_roles=?, enabled=? WHERE id=?")
        .bind(&p.name)
        .bind(&p.client_id)
        .bind(&p.client_secret_enc)
        .bind(&p.issuer)
        .bind(&p.domains)
        .bind(p.auto_create)
        .bind(&p.default_role)
        .bind(&p.group_roles)
        .bind(p.enabled)
        .bind(&id)
        .execute(&state.db)
        .await?;
    audit(&state, user.org(), Some(&user.0.id), "sso.provider_updated", &p.name).await;
    Ok(Json(p.public(&redirect_uri(&headers))))
}

async fn remove(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    user.require_admin()?;
    let p = provider(&state, &id).await?;
    if p.org_id != user.org() {
        return Err(AppError::NotFound);
    }
    sqlx::query("DELETE FROM sso_providers WHERE id = ?").bind(&id).execute(&state.db).await?;
    // With no provider left, requiring SSO would lock members out.
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sso_providers WHERE org_id = ? AND enabled = 1").bind(user.org()).fetch_one(&state.db).await?;
    if left == 0 {
        sqlx::query("UPDATE organizations SET sso_required = 0 WHERE id = ?").bind(user.org()).execute(&state.db).await?;
    }
    audit(&state, user.org(), Some(&user.0.id), "sso.provider_removed", &p.name).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct PolicyReq {
    sso_required: bool,
}

/// When required, members sign in only through SSO. Owners and admins keep their password
/// as an emergency way in (for example if the provider's configuration breaks).
async fn set_policy(State(state): State<AppState>, user: AuthUser, Json(r): Json<PolicyReq>) -> AppResult<Json<Value>> {
    user.require_admin()?;
    if r.sso_required {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sso_providers WHERE org_id = ? AND enabled = 1").bind(user.org()).fetch_one(&state.db).await?;
        if n == 0 {
            return Err(AppError::BadRequest("add and enable a sign-in provider first".into()));
        }
    }
    sqlx::query("UPDATE organizations SET sso_required = ? WHERE id = ?").bind(r.sso_required).bind(user.org()).execute(&state.db).await?;
    audit(&state, user.org(), Some(&user.0.id), if r.sso_required { "sso.required_on" } else { "sso.required_off" }, "").await;
    Ok(Json(json!({ "sso_required": r.sso_required })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(kind: &str, domains: &str) -> Provider {
        Provider {
            id: "p".into(), org_id: "o".into(), kind: kind.into(), name: "U".into(), issuer: "https://i".into(), client_id: "c".into(),
            client_secret_enc: String::new(), domains: domains.into(), auto_create: true, default_role: "viewer".into(),
            group_roles: r#"[{"group":"G-EDIT","role":"editor"},{"group":"g-admin","role":"admin"}]"#.into(), enabled: true,
        }
    }
    fn c(v: Value) -> Claims {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn microsoft_trusts_the_upn_not_the_editable_email_claim() {
        let ms = p("microsoft", "univ.ma");
        // The `email` attribute can be set to anything in Entra ID: it must not decide who you are.
        assert_eq!(trusted_email(&ms, &c(json!({ "sub": "s", "preferred_username": "Amina@Univ.ma", "email": "president@univ.ma" }))).unwrap(), "amina@univ.ma");
        // Guest accounts carry #EXT# in their UPN: refused.
        assert!(trusted_email(&ms, &c(json!({ "sub": "s", "preferred_username": "bob_gmail.com#EXT#@univ.onmicrosoft.com", "email": "x@univ.ma" }))).is_err());
        assert_eq!(trusted_email(&ms, &c(json!({ "sub": "s", "preferred_username": "a@other.ma" }))), Err("domain"));
    }

    #[test]
    fn google_requires_the_workspace_domain_and_a_verified_email() {
        let g = p("google", "univ.ma");
        assert_eq!(trusted_email(&g, &c(json!({ "sub": "s", "email": "a@univ.ma", "email_verified": true, "hd": "univ.ma" }))).unwrap(), "a@univ.ma");
        // Personal Gmail account (no hd).
        assert_eq!(trusted_email(&g, &c(json!({ "sub": "s", "email": "a@gmail.com", "email_verified": true }))), Err("domain"));
        // Another university's Workspace.
        assert_eq!(trusted_email(&g, &c(json!({ "sub": "s", "email": "a@other.ma", "email_verified": true, "hd": "other.ma" }))), Err("domain"));
        assert_eq!(trusted_email(&g, &c(json!({ "sub": "s", "email": "a@univ.ma", "hd": "univ.ma" }))), Err("email_unverified"));
    }

    #[test]
    fn highest_matching_group_wins_otherwise_default_role() {
        let pr = p("oidc", "");
        assert_eq!(role_from_groups(&pr, &c(json!({ "sub": "s", "groups": ["g-edit"] }))).as_deref(), Some("editor"));
        assert_eq!(role_from_groups(&pr, &c(json!({ "sub": "s", "groups": ["G-EDIT", "G-ADMIN"] }))).as_deref(), Some("admin"));
        assert_eq!(role_from_groups(&pr, &c(json!({ "sub": "s", "groups": ["students"] }))).as_deref(), Some("viewer"));
        let mut none = pr.clone();
        none.group_roles = "[]".into();
        assert_eq!(role_from_groups(&none, &c(json!({ "sub": "s", "groups": ["g-edit"] }))), None, "no rules: roles are managed in Qivomedia");
    }
}
