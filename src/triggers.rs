//! Triggers: external systems (POS, sensors, alarms, Zapier…) call a secret webhook URL
//! and the targeted screens instantly interrupt with a layout for N seconds.
//! Optional JSON `data` is injected into `{{placeholders}}` of text/ticker/html widgets.

use axum::{
    extract::{Path, State},
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    auth::{audit, AuthUser},
    displays::{resolve_targets, validate_target},
    error::{AppError, AppResult},
    hub::Event,
    state::AppState,
    util::{new_id, now, random_token},
};

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Trigger {
    pub id: String,
    pub org_id: String,
    pub name: String,
    pub hook_key: String,
    pub layout_id: String,
    pub duration_s: i64,
    pub target_type: String,
    pub target_id: Option<String>,
    pub last_fired: Option<String>,
    pub created_at: String,
}

const COLS: &str = "id, org_id, name, hook_key, layout_id, duration_s, target_type, target_id, last_fired, created_at";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/triggers", get(list).post(create))
        .route("/triggers/{id}", put(update).delete(remove))
        .route("/triggers/{id}/fire", post(fire_manual))
        .route("/hooks/{key}", post(fire_hook))
}

#[derive(Deserialize)]
struct Req {
    name: String,
    layout_id: String,
    #[serde(default = "d30")]
    duration_s: i64,
    target_type: String,
    target_id: Option<String>,
}
fn d30() -> i64 { 30 }

async fn validate(state: &AppState, org: &str, mut r: Req) -> AppResult<Req> {
    if r.name.trim().is_empty() {
        return Err(AppError::BadRequest("name required".into()));
    }
    if !(1..=86_400).contains(&r.duration_s) {
        return Err(AppError::BadRequest("duration must be 1–86400 s".into()));
    }
    crate::content::fetch_layout(state, org, &r.layout_id).await?;
    r.target_id = r.target_id.filter(|s| !s.is_empty());
    validate_target(state, org, &r.target_type, r.target_id.as_deref()).await?;
    Ok(r)
}

async fn list(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Vec<Trigger>>> {
    let rows = sqlx::query_as::<_, Trigger>(&format!("SELECT {COLS} FROM triggers WHERE org_id = ? ORDER BY name"))
        .bind(user.org())
        .fetch_all(&state.db)
        .await?;
    Ok(Json(rows))
}

async fn get_one(state: &AppState, org: &str, id: &str) -> AppResult<Trigger> {
    sqlx::query_as::<_, Trigger>(&format!("SELECT {COLS} FROM triggers WHERE id = ? AND org_id = ?"))
        .bind(id)
        .bind(org)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)
}

async fn create(State(state): State<AppState>, user: AuthUser, Json(req): Json<Req>) -> AppResult<Json<Trigger>> {
    user.require_editor()?;
    let r = validate(&state, user.org(), req).await?;
    let id = new_id();
    sqlx::query(&format!(
        "INSERT INTO triggers ({COLS}) VALUES (?,?,?,?,?,?,?,?,NULL,?)"
    ))
    .bind(&id)
    .bind(user.org())
    .bind(r.name.trim())
    .bind(random_token(32))
    .bind(&r.layout_id)
    .bind(r.duration_s)
    .bind(&r.target_type)
    .bind(&r.target_id)
    .bind(now())
    .execute(&state.db)
    .await?;
    state.content_changed(user.org()); // players pre-cache the trigger layout
    audit(&state, user.org(), Some(&user.0.id), "trigger.created", r.name.trim()).await;
    Ok(Json(get_one(&state, user.org(), &id).await?))
}

async fn update(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>, Json(req): Json<Req>) -> AppResult<Json<Trigger>> {
    user.require_editor()?;
    get_one(&state, user.org(), &id).await?;
    let r = validate(&state, user.org(), req).await?;
    sqlx::query("UPDATE triggers SET name=?, layout_id=?, duration_s=?, target_type=?, target_id=? WHERE id = ?")
        .bind(r.name.trim())
        .bind(&r.layout_id)
        .bind(r.duration_s)
        .bind(&r.target_type)
        .bind(&r.target_id)
        .bind(&id)
        .execute(&state.db)
        .await?;
    state.content_changed(user.org());
    Ok(Json(get_one(&state, user.org(), &id).await?))
}

async fn remove(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    user.require_editor()?;
    get_one(&state, user.org(), &id).await?;
    sqlx::query("DELETE FROM triggers WHERE id = ?").bind(&id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize, Default)]
struct FireReq {
    duration_s: Option<i64>,
    #[serde(default)]
    data: Value,
}

async fn fire(state: &AppState, t: &Trigger, req: FireReq) -> AppResult<Json<Value>> {
    let ids = resolve_targets(state, &t.org_id, &t.target_type, t.target_id.as_deref()).await?;
    let duration = req.duration_s.unwrap_or(t.duration_s).clamp(1, 86_400);
    let data = if req.data.is_object() { req.data } else { json!({}) };
    let n = ids.len();
    state.publish(Event::to_displays(
        &t.org_id,
        ids,
        json!({ "type": "interrupt", "trigger_id": t.id, "layout_id": t.layout_id, "duration_s": duration, "data": data }),
    ));
    sqlx::query("UPDATE triggers SET last_fired = ? WHERE id = ?").bind(now()).bind(&t.id).execute(&state.db).await?;
    state.publish(Event::to_admins(&t.org_id, json!({ "type": "trigger_fired", "trigger_id": t.id, "displays": n })));
    Ok(Json(json!({ "ok": true, "displays": n, "duration_s": duration })))
}

async fn fire_manual(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>, body: Option<Json<FireReq>>) -> AppResult<Json<Value>> {
    user.require_editor()?;
    let t = get_one(&state, user.org(), &id).await?;
    fire(&state, &t, body.map(|b| b.0).unwrap_or_default()).await
}

/// Public webhook — the unguessable key *is* the credential.
async fn fire_hook(State(state): State<AppState>, Path(key): Path<String>, body: Option<Json<FireReq>>) -> AppResult<Json<Value>> {
    if !crate::ratelimit::allow(&format!("hook:{key}"), 120, crate::ratelimit::MIN) {
        return Err(AppError::TooMany("trigger fired too often — max 120 per minute".into()));
    }
    let t = sqlx::query_as::<_, Trigger>(&format!("SELECT {COLS} FROM triggers WHERE hook_key = ?"))
        .bind(&key)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    fire(&state, &t, body.map(|b| b.0).unwrap_or_default()).await
}
