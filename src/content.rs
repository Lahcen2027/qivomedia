//! Playlists (ordered widget/media items) and layouts (canvas + regions bound to playlists).

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    auth::{audit, AuthUser},
    error::{AppError, AppResult},
    state::AppState,
    util::{new_id, now, valid_hhmm},
};

pub const ITEM_TYPES: &[&str] = &["media", "text", "ticker", "clock", "weather", "webpage", "html", "countdown"];
pub const TRANSITIONS: &[&str] = &["none", "fade", "slide", "zoom"];

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub items: String,
    pub shuffle: bool,
    pub updated_at: String,
    #[sqlx(default)]
    pub origin: String,
}

impl Playlist {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "name": self.name, "shuffle": self.shuffle, "updated_at": self.updated_at, "origin": self.origin,
            "items": serde_json::from_str::<Value>(&self.items).unwrap_or(json!([])),
        })
    }
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Layout {
    pub id: String,
    pub name: String,
    pub width: i64,
    pub height: i64,
    pub background: String,
    pub regions: String,
    pub updated_at: String,
    #[sqlx(default)]
    pub origin: String,
}

impl Layout {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "name": self.name, "width": self.width, "height": self.height,
            "background": self.background, "updated_at": self.updated_at, "origin": self.origin,
            "regions": serde_json::from_str::<Value>(&self.regions).unwrap_or(json!([])),
        })
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/playlists", get(list_playlists).post(create_playlist))
        .route("/playlists/{id}", get(get_playlist).put(update_playlist).delete(delete_playlist))
        .route("/playlists/{id}/duplicate", post(duplicate_playlist))
        .route("/layouts", get(list_layouts).post(create_layout))
        .route("/layouts/{id}", get(get_layout).put(update_layout).delete(delete_layout))
        .route("/layouts/{id}/duplicate", post(duplicate_layout))
}

// ---------- validation ----------

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64)
}

/// Validates + normalizes playlist items; returns the normalized JSON array.
pub(crate) async fn normalize_items(state: &AppState, org: &str, items: &[Value]) -> AppResult<Value> {
    if items.len() > 500 {
        return Err(AppError::BadRequest("a playlist can hold at most 500 items".into()));
    }
    let mut out = Vec::with_capacity(items.len());
    for (i, it) in items.iter().enumerate() {
        let mut it = it.as_object().cloned().ok_or_else(|| AppError::BadRequest(format!("item {i} must be an object")))?;
        let ty = it.get("type").and_then(Value::as_str).unwrap_or("").to_string();
        let ty = ty.as_str();
        if !ITEM_TYPES.contains(&ty) {
            return Err(AppError::BadRequest(format!("item {i}: unknown type '{ty}'")));
        }
        let dur = it.get("duration").and_then(Value::as_f64).unwrap_or(10.0);
        if !(0.5..=86_400.0).contains(&dur) {
            return Err(AppError::BadRequest(format!("item {i}: duration must be 0.5–86400 seconds")));
        }
        it.insert("duration".into(), json!(dur));
        if it.get("id").and_then(Value::as_str).map(str::is_empty).unwrap_or(true) {
            it.insert("id".into(), json!(new_id()));
        }
        if ty == "media" {
            let mid = it.get("media_id").and_then(Value::as_str).unwrap_or("");
            let ok: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media WHERE id = ? AND org_id = ?")
                .bind(mid)
                .bind(org)
                .fetch_one(&state.db)
                .await?;
            if ok == 0 {
                return Err(AppError::BadRequest(format!("item {i}: media not found")));
            }
        }
        if ty == "webpage" {
            let url = it.get("url").and_then(Value::as_str).unwrap_or("");
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err(AppError::BadRequest(format!("item {i}: webpage url must be http(s)")));
            }
        }
        if ty == "weather" && (num(&Value::Object(it.clone()), "lat").is_none() || num(&Value::Object(it.clone()), "lon").is_none()) {
            return Err(AppError::BadRequest(format!("item {i}: weather needs lat and lon")));
        }
        // Optional day-parting on a single item: { days_mask, time_from, time_to }
        if let Some(when) = it.get("when").and_then(Value::as_object) {
            for k in ["time_from", "time_to"] {
                if let Some(t) = when.get(k).and_then(Value::as_str) {
                    if !t.is_empty() && !valid_hhmm(t) {
                        return Err(AppError::BadRequest(format!("item {i}: {k} must be HH:MM")));
                    }
                }
            }
        }
        out.push(Value::Object(it));
    }
    Ok(Value::Array(out))
}

pub(crate) async fn normalize_regions(state: &AppState, org: &str, regions: &[Value]) -> AppResult<Value> {
    if regions.len() > 32 {
        return Err(AppError::BadRequest("a layout can hold at most 32 regions".into()));
    }
    let mut out = Vec::new();
    for (i, r) in regions.iter().enumerate() {
        let mut r = r.as_object().cloned().ok_or_else(|| AppError::BadRequest(format!("region {i} must be an object")))?;
        for k in ["x", "y", "w", "h"] {
            let v = r.get(k).and_then(Value::as_f64).ok_or_else(|| AppError::BadRequest(format!("region {i}: {k} required")))?;
            if !(0.0..=100.0).contains(&v) {
                return Err(AppError::BadRequest(format!("region {i}: {k} must be a percentage 0–100")));
            }
        }
        if r.get("id").and_then(Value::as_str).map(str::is_empty).unwrap_or(true) {
            r.insert("id".into(), json!(new_id()));
        }
        if !r.contains_key("z") {
            r.insert("z".into(), json!(i));
        }
        let tr = r.get("transition").and_then(Value::as_str).unwrap_or("fade").to_string();
        if !TRANSITIONS.contains(&tr.as_str()) {
            return Err(AppError::BadRequest(format!("region {i}: unknown transition")));
        }
        r.insert("transition".into(), json!(tr));
        if let Some(pid) = r.get("playlist_id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            let ok: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlists WHERE id = ? AND org_id = ?")
                .bind(pid)
                .bind(org)
                .fetch_one(&state.db)
                .await?;
            if ok == 0 {
                return Err(AppError::BadRequest(format!("region {i}: playlist not found")));
            }
        }
        out.push(Value::Object(r));
    }
    Ok(Value::Array(out))
}

// ---------- playlists ----------

const PL_COLS: &str = "id, name, items, shuffle, updated_at, origin";

async fn fetch_playlist(state: &AppState, org: &str, id: &str) -> AppResult<Playlist> {
    sqlx::query_as::<_, Playlist>(&format!("SELECT {PL_COLS} FROM playlists WHERE id = ? AND org_id = ?"))
        .bind(id)
        .bind(org)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)
}

async fn list_playlists(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Vec<Value>>> {
    let rows = sqlx::query_as::<_, Playlist>(&format!("SELECT {PL_COLS} FROM playlists WHERE org_id = ? ORDER BY name"))
        .bind(user.org())
        .fetch_all(&state.db)
        .await?;
    Ok(Json(rows.iter().map(Playlist::to_json).collect()))
}

#[derive(Deserialize)]
struct PlaylistReq {
    name: String,
    #[serde(default)]
    items: Vec<Value>,
    #[serde(default)]
    shuffle: bool,
}

async fn create_playlist(State(state): State<AppState>, user: AuthUser, Json(req): Json<PlaylistReq>) -> AppResult<Json<Value>> {
    user.require_editor()?;
    if req.name.trim().is_empty() {
        return Err(AppError::BadRequest("name required".into()));
    }
    let items = normalize_items(&state, user.org(), &req.items).await?;
    let id = new_id();
    sqlx::query("INSERT INTO playlists (id, org_id, name, items, shuffle, updated_at) VALUES (?,?,?,?,?,?)")
        .bind(&id)
        .bind(user.org())
        .bind(req.name.trim())
        .bind(items.to_string())
        .bind(req.shuffle)
        .bind(now())
        .execute(&state.db)
        .await?;
    audit(&state, user.org(), Some(&user.0.id), "playlist.created", req.name.trim()).await;
    Ok(Json(fetch_playlist(&state, user.org(), &id).await?.to_json()))
}

async fn get_playlist(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    Ok(Json(fetch_playlist(&state, user.org(), &id).await?.to_json()))
}

async fn update_playlist(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<PlaylistReq>,
) -> AppResult<Json<Value>> {
    user.require_editor()?;
    fetch_playlist(&state, user.org(), &id).await?;
    let items = normalize_items(&state, user.org(), &req.items).await?;
    sqlx::query("UPDATE playlists SET name = ?, items = ?, shuffle = ?, updated_at = ? WHERE id = ?")
        .bind(req.name.trim())
        .bind(items.to_string())
        .bind(req.shuffle)
        .bind(now())
        .bind(&id)
        .execute(&state.db)
        .await?;
    state.content_changed(user.org());
    Ok(Json(fetch_playlist(&state, user.org(), &id).await?.to_json()))
}

async fn delete_playlist(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    user.require_editor()?;
    let p = fetch_playlist(&state, user.org(), &id).await?;
    let used: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM layouts WHERE org_id = ? AND regions LIKE ?")
        .bind(user.org())
        .bind(format!("%{id}%"))
        .fetch_one(&state.db)
        .await?;
    if used > 0 {
        return Err(AppError::Conflict(format!("playlist is used by {used} layout(s)")));
    }
    sqlx::query("DELETE FROM playlists WHERE id = ?").bind(&id).execute(&state.db).await?;
    audit(&state, user.org(), Some(&user.0.id), "playlist.deleted", &p.name).await;
    Ok(Json(json!({ "ok": true })))
}

async fn duplicate_playlist(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    user.require_editor()?;
    let p = fetch_playlist(&state, user.org(), &id).await?;
    let nid = new_id();
    sqlx::query("INSERT INTO playlists (id, org_id, name, items, shuffle, updated_at) VALUES (?,?,?,?,?,?)")
        .bind(&nid)
        .bind(user.org())
        .bind(format!("{} (copy)", p.name))
        .bind(&p.items)
        .bind(p.shuffle)
        .bind(now())
        .execute(&state.db)
        .await?;
    Ok(Json(fetch_playlist(&state, user.org(), &nid).await?.to_json()))
}

// ---------- layouts ----------

const LY_COLS: &str = "id, name, width, height, background, regions, updated_at, origin";

pub async fn fetch_layout(state: &AppState, org: &str, id: &str) -> AppResult<Layout> {
    sqlx::query_as::<_, Layout>(&format!("SELECT {LY_COLS} FROM layouts WHERE id = ? AND org_id = ?"))
        .bind(id)
        .bind(org)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)
}

async fn list_layouts(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Vec<Value>>> {
    let rows = sqlx::query_as::<_, Layout>(&format!("SELECT {LY_COLS} FROM layouts WHERE org_id = ? ORDER BY name"))
        .bind(user.org())
        .fetch_all(&state.db)
        .await?;
    Ok(Json(rows.iter().map(Layout::to_json).collect()))
}

#[derive(Deserialize)]
struct LayoutReq {
    name: String,
    #[serde(default = "d_w")]
    width: i64,
    #[serde(default = "d_h")]
    height: i64,
    #[serde(default = "d_bg")]
    background: String,
    #[serde(default)]
    regions: Vec<Value>,
}
fn d_w() -> i64 { 1920 }
fn d_h() -> i64 { 1080 }
fn d_bg() -> String { "#000000".into() }

fn check_layout(req: &LayoutReq) -> AppResult<()> {
    if req.name.trim().is_empty() {
        return Err(AppError::BadRequest("name required".into()));
    }
    if !(100..=16384).contains(&req.width) || !(100..=16384).contains(&req.height) {
        return Err(AppError::BadRequest("resolution must be between 100 and 16384 px".into()));
    }
    Ok(())
}

async fn create_layout(State(state): State<AppState>, user: AuthUser, Json(req): Json<LayoutReq>) -> AppResult<Json<Value>> {
    user.require_editor()?;
    check_layout(&req)?;
    let regions = normalize_regions(&state, user.org(), &req.regions).await?;
    let id = new_id();
    sqlx::query("INSERT INTO layouts (id, org_id, name, width, height, background, regions, updated_at) VALUES (?,?,?,?,?,?,?,?)")
        .bind(&id)
        .bind(user.org())
        .bind(req.name.trim())
        .bind(req.width)
        .bind(req.height)
        .bind(&req.background)
        .bind(regions.to_string())
        .bind(now())
        .execute(&state.db)
        .await?;
    audit(&state, user.org(), Some(&user.0.id), "layout.created", req.name.trim()).await;
    Ok(Json(fetch_layout(&state, user.org(), &id).await?.to_json()))
}

async fn get_layout(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    Ok(Json(fetch_layout(&state, user.org(), &id).await?.to_json()))
}

async fn update_layout(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<LayoutReq>,
) -> AppResult<Json<Value>> {
    user.require_editor()?;
    check_layout(&req)?;
    fetch_layout(&state, user.org(), &id).await?;
    let regions = normalize_regions(&state, user.org(), &req.regions).await?;
    sqlx::query("UPDATE layouts SET name = ?, width = ?, height = ?, background = ?, regions = ?, updated_at = ? WHERE id = ?")
        .bind(req.name.trim())
        .bind(req.width)
        .bind(req.height)
        .bind(&req.background)
        .bind(regions.to_string())
        .bind(now())
        .bind(&id)
        .execute(&state.db)
        .await?;
    state.content_changed(user.org());
    Ok(Json(fetch_layout(&state, user.org(), &id).await?.to_json()))
}

async fn delete_layout(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    user.require_editor()?;
    let l = fetch_layout(&state, user.org(), &id).await?;
    // Schedules and triggers cascade; displays fall back to no default.
    sqlx::query("DELETE FROM layouts WHERE id = ?").bind(&id).execute(&state.db).await?;
    state.content_changed(user.org());
    audit(&state, user.org(), Some(&user.0.id), "layout.deleted", &l.name).await;
    Ok(Json(json!({ "ok": true })))
}

async fn duplicate_layout(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    user.require_editor()?;
    let l = fetch_layout(&state, user.org(), &id).await?;
    let nid = new_id();
    sqlx::query("INSERT INTO layouts (id, org_id, name, width, height, background, regions, updated_at) VALUES (?,?,?,?,?,?,?,?)")
        .bind(&nid)
        .bind(user.org())
        .bind(format!("{} (copy)", l.name))
        .bind(l.width)
        .bind(l.height)
        .bind(&l.background)
        .bind(&l.regions)
        .bind(now())
        .execute(&state.db)
        .await?;
    Ok(Json(fetch_layout(&state, user.org(), &nid).await?.to_json()))
}
