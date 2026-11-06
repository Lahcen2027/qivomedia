use axum::{
    extract::{Path, State},
    routing::{get, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    auth::{audit, AuthUser},
    displays::validate_target,
    error::{AppError, AppResult},
    state::AppState,
    util::{new_id, now, valid_hhmm, valid_local_dt},
};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    pub layout_id: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
    pub days_mask: i64,
    pub time_from: Option<String>,
    pub time_to: Option<String>,
    pub priority: i64,
    pub enabled: bool,
    pub created_at: String,
}

pub const SCHED_COLS: &str =
    "id, name, layout_id, target_type, target_id, starts_at, ends_at, days_mask, time_from, time_to, priority, enabled, created_at";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/schedules", get(list).post(create))
        .route("/schedules/{id}", put(update).delete(remove))
}

#[derive(Deserialize)]
struct Req {
    name: String,
    layout_id: String,
    target_type: String,
    target_id: Option<String>,
    starts_at: Option<String>,
    ends_at: Option<String>,
    #[serde(default = "all_days")]
    days_mask: i64,
    time_from: Option<String>,
    time_to: Option<String>,
    #[serde(default)]
    priority: i64,
    #[serde(default = "yes")]
    enabled: bool,
}
fn all_days() -> i64 { 127 }
fn yes() -> bool { true }

fn blank(o: Option<String>) -> Option<String> {
    o.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

async fn validate(state: &AppState, org: &str, mut r: Req) -> AppResult<Req> {
    if r.name.trim().is_empty() {
        return Err(AppError::BadRequest("name required".into()));
    }
    crate::content::fetch_layout(state, org, &r.layout_id).await?;
    r.target_id = blank(r.target_id);
    validate_target(state, org, &r.target_type, r.target_id.as_deref()).await?;
    r.starts_at = blank(r.starts_at);
    r.ends_at = blank(r.ends_at);
    r.time_from = blank(r.time_from);
    r.time_to = blank(r.time_to);
    for d in [&r.starts_at, &r.ends_at].into_iter().flatten() {
        if !valid_local_dt(d) {
            return Err(AppError::BadRequest("dates must be YYYY-MM-DDTHH:MM".into()));
        }
    }
    if let (Some(s), Some(e)) = (&r.starts_at, &r.ends_at) {
        if e <= s {
            return Err(AppError::BadRequest("end must be after start".into()));
        }
    }
    for t in [&r.time_from, &r.time_to].into_iter().flatten() {
        if !valid_hhmm(t) {
            return Err(AppError::BadRequest("daily times must be HH:MM".into()));
        }
    }
    if r.time_from.is_some() != r.time_to.is_some() {
        return Err(AppError::BadRequest("set both daily start and end time, or neither".into()));
    }
    if !(1..=127).contains(&r.days_mask) {
        return Err(AppError::BadRequest("pick at least one weekday".into()));
    }
    if !(0..=100).contains(&r.priority) {
        return Err(AppError::BadRequest("priority must be 0–100".into()));
    }
    Ok(r)
}

async fn fetch(state: &AppState, org: &str, id: &str) -> AppResult<Schedule> {
    sqlx::query_as::<_, Schedule>(&format!("SELECT {SCHED_COLS} FROM schedules WHERE id = ? AND org_id = ?"))
        .bind(id)
        .bind(org)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)
}

async fn list(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Vec<Schedule>>> {
    let rows = sqlx::query_as::<_, Schedule>(&format!(
        "SELECT {SCHED_COLS} FROM schedules WHERE org_id = ? ORDER BY priority DESC, name"
    ))
    .bind(user.org())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

async fn create(State(state): State<AppState>, user: AuthUser, Json(req): Json<Req>) -> AppResult<Json<Schedule>> {
    user.require_editor()?;
    let r = validate(&state, user.org(), req).await?;
    let id = new_id();
    sqlx::query(
        "INSERT INTO schedules (id, org_id, name, layout_id, target_type, target_id, starts_at, ends_at, days_mask,
                                time_from, time_to, priority, enabled, created_at)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(user.org())
    .bind(r.name.trim())
    .bind(&r.layout_id)
    .bind(&r.target_type)
    .bind(&r.target_id)
    .bind(&r.starts_at)
    .bind(&r.ends_at)
    .bind(r.days_mask)
    .bind(&r.time_from)
    .bind(&r.time_to)
    .bind(r.priority)
    .bind(r.enabled)
    .bind(now())
    .execute(&state.db)
    .await?;
    state.content_changed(user.org());
    audit(&state, user.org(), Some(&user.0.id), "schedule.created", r.name.trim()).await;
    Ok(Json(fetch(&state, user.org(), &id).await?))
}

async fn update(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>, Json(req): Json<Req>) -> AppResult<Json<Schedule>> {
    user.require_editor()?;
    fetch(&state, user.org(), &id).await?;
    let r = validate(&state, user.org(), req).await?;
    sqlx::query(
        "UPDATE schedules SET name=?, layout_id=?, target_type=?, target_id=?, starts_at=?, ends_at=?, days_mask=?,
                              time_from=?, time_to=?, priority=?, enabled=? WHERE id = ?",
    )
    .bind(r.name.trim())
    .bind(&r.layout_id)
    .bind(&r.target_type)
    .bind(&r.target_id)
    .bind(&r.starts_at)
    .bind(&r.ends_at)
    .bind(r.days_mask)
    .bind(&r.time_from)
    .bind(&r.time_to)
    .bind(r.priority)
    .bind(r.enabled)
    .bind(&id)
    .execute(&state.db)
    .await?;
    state.content_changed(user.org());
    Ok(Json(fetch(&state, user.org(), &id).await?))
}

async fn remove(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    user.require_editor()?;
    let s = fetch(&state, user.org(), &id).await?;
    sqlx::query("DELETE FROM schedules WHERE id = ?").bind(&id).execute(&state.db).await?;
    state.content_changed(user.org());
    audit(&state, user.org(), Some(&user.0.id), "schedule.deleted", &s.name).await;
    Ok(Json(json!({ "ok": true })))
}
