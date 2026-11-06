//! In-process API tests: every test builds the full application against its own
//! fresh SQLite database and talks to it through the router, exactly like a client.
//! Run with `cargo test`.

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use tower::ServiceExt;

use crate::{build_app, mail, state::AppState};

struct T {
    app: Router,
    ip: String,
    db: sqlx::SqlitePool,
    _dir: std::path::PathBuf,
}

async fn setup(ip: &str) -> T {
    // No network in tests: skip the online breached-password lookup (local policy still applies).
    std::env::set_var("QIVO_HIBP", "0");
    let dir = std::env::temp_dir().join(format!("qivo-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("media")).unwrap();
    std::fs::create_dir_all(dir.join("tmp")).unwrap();
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", dir.join("t.db").display()))
        .unwrap()
        .create_if_missing(true)
        .foreign_keys(true);
    let db = SqlitePoolOptions::new().max_connections(4).connect_with(opts).await.unwrap();
    sqlx::migrate!("./migrations").run(&db).await.unwrap();
    let web = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
    let app = build_app(AppState::new(db.clone(), dir.clone(), false), &web, &dir);
    T { app, ip: ip.into(), db, _dir: dir }
}

impl T {
    async fn req(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        self.req_h(method, path, token, body, &[]).await
    }
    async fn req_h(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>, extra: &[(&str, &str)]) -> (StatusCode, Value) {
        let (s, v, _) = self.req_full(method, path, token, body, extra).await;
        (s, v)
    }
    /// `token` is sent as the session cookie, like a browser does. Returns any new session token.
    async fn req_full(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>, extra: &[(&str, &str)]) -> (StatusCode, Value, Option<String>) {
        let mut b = Request::builder().method(method).uri(path).header("x-forwarded-for", &self.ip).header("host", "qivo.test");
        if let Some(t) = token {
            b = b.header("cookie", format!("qivo_session={t}"));
        }
        for (k, v) in extra {
            b = b.header(*k, *v);
        }
        let req = match body {
            Some(v) => b.header("content-type", "application/json").body(Body::from(v.to_string())).unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let session = res.headers().get_all("set-cookie").iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|c| c.strip_prefix("qivo_session=").map(|r| r.split(';').next().unwrap_or("").to_string()))
            .filter(|t| !t.is_empty());
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), session)
    }
    async fn signup(&self, email: &str) -> (String, Value) {
        let (s, v, tok) = self.req_full("POST", "/api/auth/signup", None,
            Some(json!({ "org_name": "Test Org", "name": "Owner", "email": email, "password": "correct-horse" })), &[]).await;
        assert_eq!(s, StatusCode::OK, "signup failed: {v}");
        assert!(v.get("token").is_none(), "session token must not be exposed to JavaScript");
        (tok.expect("session cookie"), v["user"].clone())
    }
    async fn login(&self, email: &str, pw: &str) -> (StatusCode, Value) {
        self.req("POST", "/api/auth/login", None, Some(json!({ "email": email, "password": pw }))).await
    }
    async fn login_token(&self, email: &str, pw: &str) -> String {
        let (s, v, tok) = self.req_full("POST", "/api/auth/login", None, Some(json!({ "email": email, "password": pw })), &[]).await;
        assert_eq!(s, StatusCode::OK, "login failed: {v}");
        tok.expect("session cookie")
    }
}

/// Pull the one-time token out of the most recent unsent email to `to`.
fn token_from_mail(to: &str) -> String {
    let m = mail::unsent().into_iter().rev().find(|m| m.to == to).unwrap_or_else(|| panic!("no email to {to}"));
    let start = m.text.find("token=").expect("no token in email") + 6;
    m.text[start..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect()
}

#[tokio::test]
async fn first_signup_is_owner_and_platform_admin_then_signup_closes() {
    let t = setup("10.0.0.1").await;
    let (_, user) = t.signup("first@test.ma").await;
    assert_eq!(user["role"], "owner");
    assert_eq!(user["platform_admin"], true);
    let (s, _) = t.req("POST", "/api/auth/signup", None,
        Some(json!({ "org_name": "Other", "name": "X", "email": "x@test.ma", "password": "correct-horse" }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn login_locks_after_repeated_failures() {
    let t = setup("10.0.0.2").await;
    t.signup("lock@test.ma").await;
    for _ in 0..5 {
        assert_eq!(t.login("lock@test.ma", "wrong-password").await.0, StatusCode::UNAUTHORIZED);
    }
    // Even the right password is refused while locked.
    assert_eq!(t.login("lock@test.ma", "correct-horse").await.0, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn successful_login_resets_failure_count() {
    let t = setup("10.0.0.3").await;
    t.signup("reset-count@test.ma").await;
    for _ in 0..4 {
        t.login("reset-count@test.ma", "nope-nope").await;
    }
    assert_eq!(t.login("reset-count@test.ma", "correct-horse").await.0, StatusCode::OK);
    for _ in 0..4 {
        t.login("reset-count@test.ma", "nope-nope").await;
    }
    assert_eq!(t.login("reset-count@test.ma", "correct-horse").await.0, StatusCode::OK);
}

#[tokio::test]
async fn password_reset_flow_is_single_use_and_signs_out_everywhere() {
    let t = setup("10.0.0.4").await;
    let (old_token, _) = t.signup("forgot@test.ma").await;

    // Unknown email: same answer, no email sent.
    let before = mail::unsent().len();
    assert_eq!(t.req("POST", "/api/auth/forgot", None, Some(json!({ "email": "nobody@test.ma" }))).await.0, StatusCode::OK);
    assert!(mail::unsent().iter().skip(before).all(|m| m.to != "nobody@test.ma"));

    assert_eq!(t.req("POST", "/api/auth/forgot", None, Some(json!({ "email": "forgot@test.ma" }))).await.0, StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await; // email is sent in the background
    let token = token_from_mail("forgot@test.ma");
    let m = mail::unsent().into_iter().rev().find(|m| m.to == "forgot@test.ma").unwrap();
    assert!(m.text.contains("https://qivo.test/#/reset?token="), "link uses the request host: {}", m.text);

    let (s, info) = t.req("POST", "/api/auth/token-info", None, Some(json!({ "token": token }))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(info["purpose"], "reset");

    assert_eq!(t.req("POST", "/api/auth/reset", None, Some(json!({ "token": token, "password": "short" }))).await.0, StatusCode::BAD_REQUEST);
    let (s, _, session) = t.req_full("POST", "/api/auth/reset", None, Some(json!({ "token": token, "password": "brand-new-pass" })), &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(session.is_some(), "reset signs the user in");

    // Old session revoked, token can't be reused, new password works, old doesn't.
    assert_eq!(t.req("GET", "/api/auth/me", Some(&old_token), None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(t.req("POST", "/api/auth/reset", None, Some(json!({ "token": token, "password": "another-pass" }))).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(t.login("forgot@test.ma", "brand-new-pass").await.0, StatusCode::OK);
    assert_eq!(t.login("forgot@test.ma", "correct-horse").await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn invitation_lets_new_member_choose_password() {
    let t = setup("10.0.0.5").await;
    let (owner, _) = t.signup("boss@test.ma").await;
    let (s, u) = t.req("POST", "/api/users", Some(&owner),
        Some(json!({ "email": "Invited@Test.ma", "name": "Amina", "role": "editor", "invite": true }))).await;
    assert_eq!(s, StatusCode::OK, "{u}");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let token = token_from_mail("invited@test.ma");
    let (_, info) = t.req("POST", "/api/auth/token-info", None, Some(json!({ "token": token }))).await;
    assert_eq!(info["purpose"], "invite");
    assert_eq!(info["name"], "Amina");
    let (s, v) = t.req("POST", "/api/auth/reset", None, Some(json!({ "token": token, "password": "my-own-pass-1" }))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["user"]["role"], "editor");
    assert_eq!(t.login("invited@test.ma", "my-own-pass-1").await.0, StatusCode::OK);
}

#[tokio::test]
async fn changing_password_signs_out_other_sessions_only() {
    let t = setup("10.0.0.6").await;
    let (a, user) = t.signup("multi@test.ma").await;
    let b = t.login_token("multi@test.ma", "correct-horse").await;
    let id = user["id"].as_str().unwrap();
    assert_eq!(t.req("PATCH", &format!("/api/users/{id}"), Some(&a), Some(json!({ "password": "changed-pass" }))).await.0, StatusCode::OK);
    assert_eq!(t.req("GET", "/api/auth/me", Some(&a), None).await.0, StatusCode::OK);
    assert_eq!(t.req("GET", "/api/auth/me", Some(&b), None).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn organizations_are_isolated() {
    let t = setup("10.0.0.7").await;
    let (admin, _) = t.signup("hq@test.ma").await;
    let (_, layout) = t.req("POST", "/api/layouts", Some(&admin), Some(json!({ "name": "HQ secret" }))).await;
    let lid = layout["id"].as_str().unwrap();
    let (s, _) = t.req("POST", "/api/platform/orgs", Some(&admin), Some(json!({
        "org_name": "Customer", "owner_name": "C", "owner_email": "c@test.ma", "owner_password": "customer-pass", "plan": "pilot" }))).await;
    assert_eq!(s, StatusCode::OK);
    let c_tok = t.login_token("c@test.ma", "customer-pass").await;
    let c = c_tok.as_str();
    assert_eq!(t.req("GET", &format!("/api/layouts/{lid}"), Some(c), None).await.0, StatusCode::NOT_FOUND);
    let (_, list) = t.req("GET", "/api/layouts", Some(c), None).await;
    assert_eq!(list.as_array().unwrap().len(), 0);
    assert_eq!(t.req("GET", "/api/platform/orgs", Some(c), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(t.req("DELETE", &format!("/api/layouts/{lid}"), Some(c), None).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn roles_are_enforced() {
    let t = setup("10.0.0.8").await;
    let (owner, _) = t.signup("roles@test.ma").await;
    for (email, role) in [("viewer@test.ma", "viewer"), ("editor@test.ma", "editor")] {
        let (s, v) = t.req("POST", "/api/users", Some(&owner), Some(json!({ "email": email, "name": role, "role": role, "password": "member-pass-1" }))).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let viewer = t.login_token("viewer@test.ma", "member-pass-1").await;
    let editor = t.login_token("editor@test.ma", "member-pass-1").await;
    assert_eq!(t.req("POST", "/api/playlists", Some(&viewer), Some(json!({ "name": "x" }))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(t.req("POST", "/api/alerts", Some(&viewer), Some(json!({ "kind": "fire" }))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(t.req("POST", "/api/playlists", Some(&editor), Some(json!({ "name": "x" }))).await.0, StatusCode::OK);
    assert_eq!(t.req("POST", "/api/displays/claim", Some(&editor), Some(json!({ "code": "AAAAAA", "name": "x" }))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(t.req("POST", "/api/users", Some(&editor), Some(json!({ "email": "z@test.ma", "name": "z", "role": "admin", "password": "member-pass-1" }))).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn emergency_alert_reaches_the_screen_manifest_and_clears() {
    let t = setup("10.0.0.9").await;
    let (owner, _) = t.signup("alerts@test.ma").await;
    let (_, reg) = t.req("POST", "/api/player/register", None, Some(json!({}))).await;
    let key = reg["device_key"].as_str().unwrap().to_string();
    let code = reg["pairing_code"].as_str().unwrap();
    assert_eq!(t.req("POST", "/api/displays/claim", Some(&owner), Some(json!({ "code": code, "name": "Lobby" }))).await.0, StatusCode::OK);
    async fn manifest_of(t: &T, key: &str) -> Value {
        t.req_h("GET", "/api/player/manifest", None, None, &[("x-device-key", key)]).await.1
    }
    assert_eq!(manifest_of(&t, &key).await["alerts"].as_array().unwrap().len(), 0);
    let (s, a) = t.req("POST", "/api/alerts", Some(&owner), Some(json!({ "kind": "fire" }))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(a["screens"], 1);
    let m = manifest_of(&t, &key).await;
    assert_eq!(m["alerts"][0]["kind"], "fire");
    assert_eq!(m["alerts"][0]["lines"].as_array().unwrap().len(), 3, "FR, AR and EN");
    assert_eq!(t.req("POST", "/api/alerts/clear-all", Some(&owner), None).await.1["cleared"], 1);
    assert_eq!(manifest_of(&t, &key).await["alerts"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn suspended_organization_cannot_sign_in() {
    let t = setup("10.0.0.10").await;
    let (admin, _) = t.signup("op@test.ma").await;
    let (_, o) = t.req("POST", "/api/platform/orgs", Some(&admin), Some(json!({
        "org_name": "Late payer", "owner_name": "L", "owner_email": "late@test.ma", "owner_password": "late-pass-123", "plan": "cloud" }))).await;
    let id = o["id"].as_str().unwrap();
    assert_eq!(t.login("late@test.ma", "late-pass-123").await.0, StatusCode::OK);
    assert_eq!(t.req("PATCH", &format!("/api/platform/orgs/{id}"), Some(&admin), Some(json!({ "suspended": true }))).await.0, StatusCode::OK);
    assert_eq!(t.login("late@test.ma", "late-pass-123").await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn website_leads_are_validated_and_notify_platform_admins() {
    let t = setup("10.0.0.11").await;
    let (admin, _) = t.signup("leads-admin@test.ma").await;
    let (s, _) = t.req("POST", "/api/public/demo-requests", None, Some(json!({ "name": "Bot", "email": "b@b.co", "website": "spam" }))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t.req("POST", "/api/public/demo-requests", None, Some(json!({ "name": "X", "email": "not-an-email" }))).await.0, StatusCode::BAD_REQUEST);
    let (s, _) = t.req("POST", "/api/public/demo-requests", None, Some(json!({ "name": "Amina", "email": "amina@fs.ma", "organization": "FS", "screens": "11–50" }))).await;
    assert_eq!(s, StatusCode::OK);
    let (_, leads) = t.req("GET", "/api/platform/leads", Some(&admin), None).await;
    assert_eq!(leads.as_array().unwrap().len(), 1, "honeypot submission is not stored");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(mail::unsent().iter().any(|m| m.to == "leads-admin@test.ma" && m.subject.contains("Amina")));
}

#[tokio::test]
async fn security_headers_are_set() {
    let t = setup("10.0.0.12").await;
    let get = |path: &'static str| {
        let app = t.app.clone();
        async move { app.oneshot(Request::builder().uri(path).header("host", "qivo.test").body(Body::empty()).unwrap()).await.unwrap() }
    };
    let api = get("/api/health").await;
    let h = api.headers();
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(h.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(h.get("cache-control").unwrap(), "no-store");
    assert!(h.get("content-security-policy").unwrap().to_str().unwrap().contains("default-src 'none'"));
    assert!(h.get("permissions-policy").unwrap().to_str().unwrap().contains("camera=()"));
    assert!(h.get("access-control-allow-origin").is_none(), "no permissive CORS");
    let app = get("/").await;
    let csp = app.headers().get("content-security-policy").unwrap().to_str().unwrap().to_string();
    assert!(csp.contains("script-src 'self';") && !csp.contains("script-src 'self' 'unsafe-inline'"), "dashboard forbids inline script: {csp}");
    assert_eq!(app.headers().get("cross-origin-opener-policy").unwrap(), "same-origin");
    let txt = get("/.well-known/security.txt").await;
    assert_eq!(txt.status(), StatusCode::OK);
    let body = String::from_utf8(txt.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(body.contains("Contact:") && body.contains("Expires:"));
}


// ------------------------------------------------------------------ hardening tests

fn totp_code(secret_b32: &str, at: u64) -> String {
    let bytes = totp_rs::Secret::Encoded(secret_b32.to_string()).to_bytes().unwrap();
    totp_rs::TOTP::new(totp_rs::Algorithm::SHA1, 6, 1, 30, bytes, Some("Qivomedia".into()), "x@y.co".into()).unwrap().generate(at)
}
fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

#[tokio::test]
async fn two_factor_enrollment_login_replay_and_recovery() {
    let t = setup("10.1.0.1").await;
    let (tok, _) = t.signup("mfa@test.ma").await;
    let (s, setup) = t.req("POST", "/api/auth/mfa/setup", Some(&tok), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(setup["qr_png_base64"].as_str().unwrap().len() > 100);
    let secret = setup["secret"].as_str().unwrap().to_string();
    assert_eq!(t.req("POST", "/api/auth/mfa/enable", Some(&tok), Some(json!({ "code": "000000" }))).await.0, StatusCode::BAD_REQUEST);
    let (s, en) = t.req("POST", "/api/auth/mfa/enable", Some(&tok), Some(json!({ "code": totp_code(&secret, unix_now()) }))).await;
    assert_eq!(s, StatusCode::OK, "{en}");
    let codes: Vec<String> = serde_json::from_value(en["recovery_codes"].clone()).unwrap();
    assert_eq!(codes.len(), 10);

    // Password alone no longer signs in.
    let (s, l, sess) = t.req_full("POST", "/api/auth/login", None, Some(json!({ "email": "mfa@test.ma", "password": "correct-horse" })), &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(l["mfa_required"], true);
    assert!(sess.is_none(), "no session before the second factor");
    let ticket = l["ticket"].as_str().unwrap().to_string();
    // The enable step already used the current time step: the same code is a replay.
    let replay = totp_code(&secret, unix_now());
    assert_eq!(t.req("POST", "/api/auth/mfa/verify", None, Some(json!({ "ticket": ticket, "code": replay }))).await.0, StatusCode::BAD_REQUEST);
    // Next step's code (within allowed drift) works.
    let (s, _, sess) = t.req_full("POST", "/api/auth/mfa/verify", None, Some(json!({ "ticket": ticket, "code": totp_code(&secret, unix_now() + 30) })), &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(sess.is_some());
    // Ticket is single-use.
    assert_eq!(t.req("POST", "/api/auth/mfa/verify", None, Some(json!({ "ticket": ticket, "code": "123456" }))).await.0, StatusCode::BAD_REQUEST);

    // Recovery code works exactly once.
    let l2 = t.req("POST", "/api/auth/login", None, Some(json!({ "email": "mfa@test.ma", "password": "correct-horse" }))).await.1;
    let (s, _) = t.req("POST", "/api/auth/mfa/verify", None, Some(json!({ "ticket": l2["ticket"], "code": codes[0].to_uppercase() }))).await;
    assert_eq!(s, StatusCode::OK, "recovery codes are case-insensitive");
    let l3 = t.req("POST", "/api/auth/login", None, Some(json!({ "email": "mfa@test.ma", "password": "correct-horse" }))).await.1;
    assert_eq!(t.req("POST", "/api/auth/mfa/verify", None, Some(json!({ "ticket": l3["ticket"], "code": codes[0] }))).await.0, StatusCode::BAD_REQUEST);

    // A password-reset link must not bypass 2FA.
    t.req("POST", "/api/auth/forgot", None, Some(json!({ "email": "mfa@test.ma" }))).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let rt = token_from_mail("mfa@test.ma");
    let (s, v, sess) = t.req_full("POST", "/api/auth/reset", None, Some(json!({ "token": rt, "password": "a-whole-new-phrase" })), &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["signin_required"], true);
    assert!(sess.is_none());
}

#[tokio::test]
async fn organization_can_require_two_factor() {
    let t = setup("10.1.0.2").await;
    let (owner, _) = t.signup("req@test.ma").await;
    t.req("POST", "/api/users", Some(&owner), Some(json!({ "email": "m@test.ma", "name": "Member", "role": "editor", "password": "pancake-river-77" }))).await;
    // Owner must have 2FA before requiring it.
    assert_eq!(t.req("PATCH", "/api/org/security", Some(&owner), Some(json!({ "require_mfa": true }))).await.0, StatusCode::BAD_REQUEST);
    let secret = t.req("POST", "/api/auth/mfa/setup", Some(&owner), None).await.1["secret"].as_str().unwrap().to_string();
    t.req("POST", "/api/auth/mfa/enable", Some(&owner), Some(json!({ "code": totp_code(&secret, unix_now()) }))).await;
    assert_eq!(t.req("PATCH", "/api/org/security", Some(&owner), Some(json!({ "require_mfa": true }))).await.0, StatusCode::OK);

    let member = t.login_token("m@test.ma", "pancake-river-77").await;
    let (_, me) = t.req("GET", "/api/auth/me", Some(&member), None).await;
    assert_eq!(me["user"]["mfa_setup_required"], true);
    let (s, v) = t.req("GET", "/api/layouts", Some(&member), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(v["error"].as_str().unwrap().starts_with("mfa_setup_required"));
    let ms = t.req("POST", "/api/auth/mfa/setup", Some(&member), None).await.1["secret"].as_str().unwrap().to_string();
    assert_eq!(t.req("POST", "/api/auth/mfa/enable", Some(&member), Some(json!({ "code": totp_code(&ms, unix_now()) }))).await.0, StatusCode::OK);
    assert_eq!(t.req("GET", "/api/layouts", Some(&member), None).await.0, StatusCode::OK);
    // Can't switch it off while the organization requires it.
    let (s, _) = t.req("POST", "/api/auth/mfa/disable", Some(&member), Some(json!({ "password": "pancake-river-77", "code": totp_code(&ms, unix_now() + 30) }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cross_site_requests_are_blocked() {
    let t = setup("10.1.0.3").await;
    let (tok, _) = t.signup("csrf@test.ma").await;
    let body = Some(json!({ "name": "x" }));
    let (s, v) = t.req_h("POST", "/api/playlists", Some(&tok), body.clone(), &[("origin", "https://evil.example")]).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(t.req_h("POST", "/api/playlists", Some(&tok), body.clone(), &[("sec-fetch-site", "cross-site")]).await.0, StatusCode::FORBIDDEN);
    assert_eq!(t.req_h("POST", "/api/playlists", Some(&tok), body.clone(), &[("origin", "http://qivo.test")]).await.0, StatusCode::OK);
    // Public webhooks are meant to be called from other systems.
    assert_ne!(t.req_h("POST", "/api/public/demo-requests", None, Some(json!({ "name": "A", "email": "a@b.co" })), &[("origin", "https://elsewhere.example")]).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn sessions_expire_when_idle_and_can_be_revoked() {
    let t = setup("10.1.0.4").await;
    let (a, _) = t.signup("sess@test.ma").await;
    let b = t.login_token("sess@test.ma", "correct-horse").await;
    let (_, list) = t.req("GET", "/api/auth/sessions", Some(&a), None).await;
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list.iter().filter(|s| s["current"] == true).count(), 1);
    let other = list.iter().find(|s| s["current"] == false).unwrap()["id"].as_str().unwrap().to_string();
    assert_eq!(t.req("DELETE", &format!("/api/auth/sessions/{other}"), Some(&a), None).await.0, StatusCode::OK);
    assert_eq!(t.req("GET", "/api/auth/me", Some(&b), None).await.0, StatusCode::UNAUTHORIZED);
    // Idle for longer than the limit → signed out.
    sqlx::query("UPDATE sessions SET last_used_at = '2000-01-01T00:00:00Z'").execute(&t.db).await.unwrap();
    assert_eq!(t.req("GET", "/api/auth/me", Some(&a), None).await.0, StatusCode::UNAUTHORIZED);
    // Logout clears the cookie.
    let c = t.login_token("sess@test.ma", "correct-horse").await;
    let res = t.app.clone().oneshot(Request::builder().method("POST").uri("/api/auth/logout").header("cookie", format!("qivo_session={c}")).body(Body::empty()).unwrap()).await.unwrap();
    assert!(res.headers().get_all("set-cookie").iter().any(|v| v.to_str().unwrap().contains("Max-Age=0")));
    assert_eq!(t.req("GET", "/api/auth/me", Some(&c), None).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn weak_and_contextual_passwords_are_rejected() {
    let t = setup("10.1.0.5").await;
    let (owner, _) = t.signup("pw@test.ma").await;
    for (pw, why) in [("short-pw", "too short"), ("Password1234", "common"), ("aaaaaaaaaaaaaaa", "repeated"), ("nadia-loves-signage", "built from name")] {
        let (s, v) = t.req("POST", "/api/users", Some(&owner), Some(json!({ "email": "nadia@test.ma", "name": "Nadia", "role": "editor", "password": pw }))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{why}: {v}");
    }
    assert_eq!(t.req("POST", "/api/users", Some(&owner), Some(json!({ "email": "nadia@test.ma", "name": "Nadia", "role": "editor", "password": "tea garden bicycle" }))).await.0, StatusCode::OK);
}

#[tokio::test]
async fn uploads_are_checked_by_content_and_active_files_are_sandboxed() {
    let t = setup("10.1.0.6").await;
    let (tok, _) = t.signup("up@test.ma").await;
    let upload = |name: &'static str, ctype: &'static str, bytes: Vec<u8>| {
        let app = t.app.clone();
        let tok = tok.clone();
        async move {
            let bd = "XQIVOBOUNDARY";
            let mut body = format!("--{bd}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {ctype}\r\n\r\n").into_bytes();
            body.extend(bytes);
            body.extend(format!("\r\n--{bd}--\r\n").into_bytes());
            let res = app.oneshot(Request::builder().method("POST").uri("/api/media").header("host", "qivo.test")
                .header("cookie", format!("qivo_session={tok}")).header("content-type", format!("multipart/form-data; boundary={bd}"))
                .body(Body::from(body)).unwrap()).await.unwrap();
            let s = res.status();
            let v: Value = serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap_or(Value::Null);
            (s, v)
        }
    };
    let (s, _) = upload("invoice.png", "image/png", b"MZ\x90\x00\x03\x00\x00\x00\x04\x00\x00\x00\xff\xff\x00\x00".to_vec()).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "executable disguised as PNG is refused");
    let (s, v) = upload("logo.svg", "image/svg+xml", b"<svg xmlns='http://www.w3.org/2000/svg' onload='alert(1)'><script>alert(1)</script></svg>".to_vec()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let url = v[0]["url"].as_str().unwrap().to_string();
    let res = t.app.clone().oneshot(Request::builder().uri(&url).header("host", "qivo.test").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let csp = res.headers().get("content-security-policy").unwrap().to_str().unwrap();
    assert!(csp.starts_with("sandbox"), "SVG is served sandboxed: {csp}");
}

#[tokio::test]
async fn platform_admin_can_recover_a_customer_who_lost_their_phone() {
    let t = setup("10.1.0.7").await;
    let (op, _) = t.signup("operator@test.ma").await;
    t.req("POST", "/api/platform/orgs", Some(&op), Some(json!({
        "org_name": "Faculty", "owner_name": "Salma", "owner_email": "salma@fs.ma", "owner_password": "river stone garden", "plan": "pilot" }))).await;
    // Customer owner turns on 2FA, then "loses the phone".
    let cust = t.login_token("salma@fs.ma", "river stone garden").await;
    let cs = t.req("POST", "/api/auth/mfa/setup", Some(&cust), None).await.1["secret"].as_str().unwrap().to_string();
    assert_eq!(t.req("POST", "/api/auth/mfa/enable", Some(&cust), Some(json!({ "code": totp_code(&cs, unix_now()) }))).await.0, StatusCode::OK);
    let (_, cme) = t.req("GET", "/api/auth/me", Some(&cust), None).await;
    let cust_id = cme["user"]["id"].as_str().unwrap().to_string();
    let org_id = cme["user"]["org_id"].as_str().unwrap().to_string();

    // The operator must protect their own account first.
    let (s, _) = t.req("POST", &format!("/api/platform/users/{cust_id}/reset-mfa"), Some(&op), Some(json!({ "reason": "Called her on the contract number" }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let os = t.req("POST", "/api/auth/mfa/setup", Some(&op), None).await.1["secret"].as_str().unwrap().to_string();
    t.req("POST", "/api/auth/mfa/enable", Some(&op), Some(json!({ "code": totp_code(&os, unix_now()) }))).await;

    let (_, users) = t.req("GET", &format!("/api/platform/orgs/{org_id}/users"), Some(&op), None).await;
    assert_eq!(users[0]["email"], "salma@fs.ma");
    assert_eq!(users[0]["mfa_enabled"], true);
    assert_eq!(t.req("POST", &format!("/api/platform/users/{cust_id}/reset-mfa"), Some(&op), Some(json!({ "reason": "ok" }))).await.0, StatusCode::BAD_REQUEST, "a real reason is required");
    let (s, v) = t.req("POST", &format!("/api/platform/users/{cust_id}/reset-mfa"), Some(&op), Some(json!({ "reason": "Called her on the number in our contract" }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // Old sessions are gone, password alone signs in again, she was told, and it's in her org's audit log.
    assert_eq!(t.req("GET", "/api/auth/me", Some(&cust), None).await.0, StatusCode::UNAUTHORIZED);
    let (s, l) = t.login("salma@fs.ma", "river stone garden").await;
    assert_eq!(s, StatusCode::OK);
    assert!(l.get("mfa_required").is_none());
    assert!(mail::unsent().iter().any(|m| m.to == "salma@fs.ma" && m.subject.contains("two-factor authentication was reset")));
    let logged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE org_id = ? AND action = 'auth.mfa_reset_by_platform' AND detail LIKE '%contract%'")
        .bind(&org_id).fetch_one(&t.db).await.unwrap();
    assert_eq!(logged, 1);

    // Not for yourself, and not for customers' own admins.
    let (_, ome) = t.req("GET", "/api/auth/me", Some(&op), None).await;
    let op_id = ome["user"]["id"].as_str().unwrap();
    assert_eq!(t.req("POST", &format!("/api/platform/users/{op_id}/reset-mfa"), Some(&op), Some(json!({ "reason": "I lost my own phone today" }))).await.0, StatusCode::BAD_REQUEST);
    let cust2 = t.login_token("salma@fs.ma", "river stone garden").await;
    assert_eq!(t.req("GET", &format!("/api/platform/orgs/{org_id}/users"), Some(&cust2), None).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn organization_admin_reset_notifies_the_member() {
    let t = setup("10.1.0.8").await;
    let (owner, _) = t.signup("own@test.ma").await;
    let (_, u) = t.req("POST", "/api/users", Some(&owner), Some(json!({ "email": "yassine@test.ma", "name": "Yassine", "role": "editor", "password": "copper kettle dawn" }))).await;
    let member = t.login_token("yassine@test.ma", "copper kettle dawn").await;
    let ms = t.req("POST", "/api/auth/mfa/setup", Some(&member), None).await.1["secret"].as_str().unwrap().to_string();
    t.req("POST", "/api/auth/mfa/enable", Some(&member), Some(json!({ "code": totp_code(&ms, unix_now()) }))).await;
    let uid = u["id"].as_str().unwrap();
    assert_eq!(t.req("PATCH", &format!("/api/users/{uid}"), Some(&owner), Some(json!({ "reset_mfa": true }))).await.0, StatusCode::OK);
    assert_eq!(t.req("GET", "/api/auth/me", Some(&member), None).await.0, StatusCode::UNAUTHORIZED);
    assert!(mail::unsent().iter().any(|m| m.to == "yassine@test.ma" && m.text.contains("administrator of your organization")));
}

#[tokio::test]
async fn server_side_reset_works_for_anyone_including_the_platform_admin() {
    let t = setup("10.1.0.9").await;
    let (op, me) = t.signup("root@test.ma").await;
    let s = t.req("POST", "/api/auth/mfa/setup", Some(&op), None).await.1["secret"].as_str().unwrap().to_string();
    t.req("POST", "/api/auth/mfa/enable", Some(&op), Some(json!({ "code": totp_code(&s, unix_now()) }))).await;
    // What `qivo-server reset-mfa root@test.ma` runs.
    let (email, _, _) = crate::mfa::reset_for_user(&t.db, me["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(email, "root@test.ma");
    let (s, l) = t.login("root@test.ma", "correct-horse").await;
    assert_eq!(s, StatusCode::OK);
    assert!(l.get("mfa_required").is_none());
}
