//! Public endpoints for the marketing website: demo requests (leads) and the live
//! demo layout shown in the website's hero. No authentication, so inputs are
//! validated strictly and rate-limited per IP.

use std::{
    collections::{BTreeSet, HashMap},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    routing::{get, patch, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    auth::{audit, AuthUser},
    error::{AppError, AppResult},
    state::AppState,
    util::{new_id, now},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/public/demo-requests", post(create_lead))
        .route("/public/demo", get(demo))
        .route("/platform/leads", get(list_leads))
        .route("/platform/leads/{id}", patch(update_lead))
        .route("/platform/settings/demo-layout", put(set_demo_layout).get(get_demo_layout))
}

// ------------------------------------------------------------ rate limiting

fn limiter() -> &'static Mutex<HashMap<String, Vec<Instant>>> {
    static L: OnceLock<Mutex<HashMap<String, Vec<Instant>>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// At most `max` hits per `window` per key. Old entries are pruned on every call.
fn allow(key: &str, max: usize, window: Duration) -> bool {
    let mut m = limiter().lock().unwrap();
    let now = Instant::now();
    m.retain(|_, v| {
        v.retain(|t| now.duration_since(*t) < window);
        !v.is_empty()
    });
    let hits = m.entry(key.to_string()).or_default();
    if hits.len() >= max {
        return false;
    }
    hits.push(now);
    true
}

fn client_ip(h: &HeaderMap) -> String {
    h.get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local".into())
}

// ------------------------------------------------------------ leads

#[derive(Deserialize)]
struct LeadReq {
    name: String,
    email: String,
    #[serde(default)]
    organization: String,
    #[serde(default)]
    screens: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    lang: String,
    /// Honeypot: real visitors never see or fill this field.
    #[serde(default)]
    website: String,
}

fn clip(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

async fn create_lead(State(state): State<AppState>, headers: HeaderMap, Json(r): Json<LeadReq>) -> AppResult<Json<Value>> {
    let ip = client_ip(&headers);
    if !r.website.is_empty() {
        // Pretend success to bots.
        return Ok(Json(json!({ "ok": true })));
    }
    if !allow(&format!("lead:{ip}"), 5, Duration::from_secs(3600)) {
        return Err(AppError::BadRequest("too many requests — please try again later or email us".into()));
    }
    let email = clip(&r.email, 200).to_lowercase();
    let valid_email = email.split_once('@').map(|(a, d)| !a.is_empty() && d.contains('.') && !d.starts_with('.')).unwrap_or(false);
    if clip(&r.name, 120).is_empty() || !valid_email {
        return Err(AppError::BadRequest("please enter your name and a valid email".into()));
    }
    let lang = match r.lang.as_str() { "fr" | "ar" => r.lang.clone(), _ => "en".into() };
    sqlx::query("INSERT INTO leads (id, name, email, organization, screens, message, lang, ip, created_at) VALUES (?,?,?,?,?,?,?,?,?)")
        .bind(new_id()).bind(clip(&r.name, 120)).bind(&email).bind(clip(&r.organization, 160))
        .bind(clip(&r.screens, 20)).bind(clip(&r.message, 2000)).bind(&lang).bind(&ip).bind(now())
        .execute(&state.db).await?;
    tracing::info!("new demo request from {email}");
    Ok(Json(json!({ "ok": true })))
}

#[derive(Serialize, sqlx::FromRow)]
struct Lead {
    id: String,
    name: String,
    email: String,
    organization: String,
    screens: String,
    message: String,
    lang: String,
    status: String,
    created_at: String,
}

async fn list_leads(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Vec<Lead>>> {
    user.require_platform()?;
    let rows = sqlx::query_as::<_, Lead>(
        "SELECT id, name, email, organization, screens, message, lang, status, created_at FROM leads ORDER BY created_at DESC LIMIT 500",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
struct LeadUpdate {
    status: String,
}

async fn update_lead(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>, Json(r): Json<LeadUpdate>) -> AppResult<Json<Value>> {
    user.require_platform()?;
    if !["new", "contacted", "won", "lost"].contains(&r.status.as_str()) {
        return Err(AppError::BadRequest("status must be new, contacted, won or lost".into()));
    }
    let res = sqlx::query("UPDATE leads SET status = ? WHERE id = ?").bind(&r.status).bind(&id).execute(&state.db).await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------ website demo layout

async fn setting(state: &AppState, key: &str) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar("SELECT value FROM platform_settings WHERE key = ?").bind(key).fetch_optional(&state.db).await?)
}

async fn put_setting(state: &AppState, key: &str, value: Option<&str>) -> AppResult<()> {
    match value {
        Some(v) => sqlx::query("INSERT INTO platform_settings (key, value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
            .bind(key).bind(v).execute(&state.db).await?,
        None => sqlx::query("DELETE FROM platform_settings WHERE key = ?").bind(key).execute(&state.db).await?,
    };
    Ok(())
}

#[derive(Deserialize)]
struct DemoReq {
    layout_id: Option<String>,
}

async fn set_demo_layout(State(state): State<AppState>, user: AuthUser, Json(r): Json<DemoReq>) -> AppResult<Json<Value>> {
    user.require_platform()?;
    match r.layout_id.filter(|s| !s.is_empty()) {
        Some(lid) => {
            crate::content::fetch_layout(&state, user.org(), &lid).await?; // must be in the operator's own org
            put_setting(&state, "demo_layout", Some(&lid)).await?;
            put_setting(&state, "demo_org", Some(user.org())).await?;
            audit(&state, user.org(), Some(&user.0.id), "platform.demo_layout_set", &lid).await;
            Ok(Json(json!({ "layout_id": lid })))
        }
        None => {
            put_setting(&state, "demo_layout", None).await?;
            Ok(Json(json!({ "layout_id": null })))
        }
    }
}

async fn get_demo_layout(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    user.require_platform()?;
    Ok(Json(json!({ "layout_id": setting(&state, "demo_layout").await? })))
}

/// The website hero's live player loads this: one layout bundle, read-only.
async fn demo(State(state): State<AppState>, headers: HeaderMap) -> AppResult<Json<Value>> {
    if !allow(&format!("demo:{}", client_ip(&headers)), 120, Duration::from_secs(60)) {
        return Err(AppError::BadRequest("slow down".into()));
    }
    // No demo chosen yet: the player falls back to its built-in demo.
    let (Some(lid), Some(org)) = (setting(&state, "demo_layout").await?, setting(&state, "demo_org").await?) else {
        return Ok(Json(json!({ "status": "none" })));
    };
    let ids = BTreeSet::from([lid.clone()]);
    let (layouts, playlists, media) = crate::player::bundle(&state, &org, &ids).await?;
    if layouts.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        return Ok(Json(json!({ "status": "none" })));
    }
    Ok(Json(json!({
        "status": "active", "version": "demo", "display": { "id": "demo", "name": "Demo" },
        "default_layout_id": lid, "schedules": [], "alerts": [], "layouts": layouts, "playlists": playlists, "media": media,
    })))
}

#[cfg(test)]
mod tests {
    use super::allow;
    use std::time::Duration;
    #[test]
    fn rate_limit() {
        for _ in 0..3 {
            assert!(allow("t:1", 3, Duration::from_secs(60)));
        }
        assert!(!allow("t:1", 3, Duration::from_secs(60)));
        assert!(allow("t:2", 3, Duration::from_secs(60)));
    }
}
