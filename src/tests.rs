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
    _dir: std::path::PathBuf,
}

async fn setup(ip: &str) -> T {
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
    let app = build_app(AppState::new(db, dir.clone(), false), &web, &dir);
    T { app, ip: ip.into(), _dir: dir }
}

impl T {
    async fn req(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        self.req_h(method, path, token, body, &[]).await
    }
    async fn req_h(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>, extra: &[(&str, &str)]) -> (StatusCode, Value) {
        let mut b = Request::builder().method(method).uri(path).header("x-forwarded-for", &self.ip).header("host", "qivo.test");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
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
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
    async fn signup(&self, email: &str) -> (String, Value) {
        let (s, v) = self.req("POST", "/api/auth/signup", None,
            Some(json!({ "org_name": "Test Org", "name": "Owner", "email": email, "password": "correct-horse" }))).await;
        assert_eq!(s, StatusCode::OK, "signup failed: {v}");
        (v["token"].as_str().unwrap().to_string(), v["user"].clone())
    }
    async fn login(&self, email: &str, pw: &str) -> (StatusCode, Value) {
        self.req("POST", "/api/auth/login", None, Some(json!({ "email": email, "password": pw }))).await
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
    let (s, v) = t.req("POST", "/api/auth/reset", None, Some(json!({ "token": token, "password": "brand-new-pass" }))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["token"].is_string(), "reset signs the user in");

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
    let (s, v) = t.req("POST", "/api/auth/reset", None, Some(json!({ "token": token, "password": "my-own-pass" }))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["user"]["role"], "editor");
    assert_eq!(t.login("invited@test.ma", "my-own-pass").await.0, StatusCode::OK);
}

#[tokio::test]
async fn changing_password_signs_out_other_sessions_only() {
    let t = setup("10.0.0.6").await;
    let (a, user) = t.signup("multi@test.ma").await;
    let (_, l) = t.login("multi@test.ma", "correct-horse").await;
    let b = l["token"].as_str().unwrap().to_string();
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
    let (_, l) = t.login("c@test.ma", "customer-pass").await;
    let c = l["token"].as_str().unwrap();
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
        let (s, v) = t.req("POST", "/api/users", Some(&owner), Some(json!({ "email": email, "name": role, "role": role, "password": "member-pass" }))).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let viewer = t.login("viewer@test.ma", "member-pass").await.1["token"].as_str().unwrap().to_string();
    let editor = t.login("editor@test.ma", "member-pass").await.1["token"].as_str().unwrap().to_string();
    assert_eq!(t.req("POST", "/api/playlists", Some(&viewer), Some(json!({ "name": "x" }))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(t.req("POST", "/api/alerts", Some(&viewer), Some(json!({ "kind": "fire" }))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(t.req("POST", "/api/playlists", Some(&editor), Some(json!({ "name": "x" }))).await.0, StatusCode::OK);
    assert_eq!(t.req("POST", "/api/displays/claim", Some(&editor), Some(json!({ "code": "AAAAAA", "name": "x" }))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(t.req("POST", "/api/users", Some(&editor), Some(json!({ "email": "z@test.ma", "name": "z", "role": "admin", "password": "member-pass" }))).await.0, StatusCode::FORBIDDEN);
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
        "org_name": "Late payer", "owner_name": "L", "owner_email": "late@test.ma", "owner_password": "late-pass-1", "plan": "cloud" }))).await;
    let id = o["id"].as_str().unwrap();
    assert_eq!(t.login("late@test.ma", "late-pass-1").await.0, StatusCode::OK);
    assert_eq!(t.req("PATCH", &format!("/api/platform/orgs/{id}"), Some(&admin), Some(json!({ "suspended": true }))).await.0, StatusCode::OK);
    assert_eq!(t.login("late@test.ma", "late-pass-1").await.0, StatusCode::FORBIDDEN);
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
    let res = t.app.clone().oneshot(Request::builder().uri("/api/health").body(Body::empty()).unwrap()).await.unwrap();
    let h = res.headers();
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(h.get("x-frame-options").unwrap(), "SAMEORIGIN");
    assert!(h.get("referrer-policy").is_some());
}
