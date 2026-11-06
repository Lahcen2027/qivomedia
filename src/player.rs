//! Player protocol: registration/pairing, manifest, proof-of-play and the push socket.
//!
//! The manifest carries *all* schedules for the display (not just "what plays now"),
//! so a player keeps switching layouts correctly while fully offline.

use std::collections::{BTreeMap, BTreeSet};

use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, FromRequestParts, Path, State},
    http::request::Parts,
    response::Response,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    auth::AuthUser,
    content::Playlist,
    error::{AppError, AppResult},
    hub::Event,
    media::{Media, MEDIA_COLS},
    schedules::{Schedule, SCHED_COLS},
    state::AppState,
    util::{new_id, now, pairing_code, random_token, sha256_hex},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/player/register", post(register))
        .route("/player/manifest", get(manifest))
        .route("/player/pop", post(proof_of_play))
        .route("/preview/layout/{id}", get(preview_layout))
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Device {
    pub id: String,
    pub org_id: Option<String>,
    pub name: String,
    pub status: String,
    pub pairing_code: Option<String>,
    pub group_id: Option<String>,
    pub default_layout_id: Option<String>,
}

fn key_from_parts(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get("x-device-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| parts.uri.query().and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("key=").map(str::to_owned))))
}

impl FromRequestParts<AppState> for Device {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let key = key_from_parts(parts).ok_or(AppError::Unauthorized)?;
        sqlx::query_as::<_, Device>(
            "SELECT id, org_id, name, status, pairing_code, group_id, default_layout_id FROM displays WHERE device_key_hash = ?",
        )
        .bind(sha256_hex(&key))
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::Unauthorized)
    }
}

#[derive(Deserialize, Default)]
struct RegisterReq {
    #[serde(default)]
    info: Value,
}

async fn register(State(state): State<AppState>, body: Option<Json<RegisterReq>>) -> AppResult<Json<Value>> {
    let info = body.map(|b| b.0.info).filter(Value::is_object).unwrap_or(json!({}));
    let id = new_id();
    let key = random_token(48);
    // Retry on the (unlikely) pairing-code collision.
    for _ in 0..5 {
        let code = pairing_code();
        let res = sqlx::query(
            "INSERT INTO displays (id, org_id, name, device_key_hash, pairing_code, status, player_info, created_at)
             VALUES (?, NULL, ?, ?, ?, 'pending', ?, ?)",
        )
        .bind(&id)
        .bind(format!("Screen {code}"))
        .bind(sha256_hex(&key))
        .bind(&code)
        .bind(info.to_string())
        .bind(now())
        .execute(&state.db)
        .await;
        match res {
            Ok(_) => return Ok(Json(json!({ "display_id": id, "device_key": key, "pairing_code": code }))),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(AppError::Other(anyhow::anyhow!("could not allocate pairing code")))
}

/// Gather layouts → playlists → media into one self-contained bundle.
pub(crate) async fn bundle(state: &AppState, org: &str, layout_ids: &BTreeSet<String>) -> AppResult<(Value, Value, Value)> {
    let mut layouts = BTreeMap::new();
    let mut playlist_ids = BTreeSet::new();
    for lid in layout_ids {
        if let Ok(l) = crate::content::fetch_layout(state, org, lid).await {
            let j = l.to_json();
            for r in j["regions"].as_array().into_iter().flatten() {
                if let Some(p) = r["playlist_id"].as_str().filter(|s| !s.is_empty()) {
                    playlist_ids.insert(p.to_string());
                }
            }
            layouts.insert(lid.clone(), j);
        }
    }
    let mut playlists = BTreeMap::new();
    let mut media_ids = BTreeSet::new();
    for pid in &playlist_ids {
        let p: Option<Playlist> = sqlx::query_as("SELECT id, name, items, shuffle, updated_at FROM playlists WHERE id = ? AND org_id = ?")
            .bind(pid)
            .bind(org)
            .fetch_optional(&state.db)
            .await?;
        if let Some(p) = p {
            let j = p.to_json();
            for it in j["items"].as_array().into_iter().flatten() {
                if let Some(m) = it["media_id"].as_str() {
                    media_ids.insert(m.to_string());
                }
            }
            playlists.insert(pid.clone(), j);
        }
    }
    let mut media = BTreeMap::new();
    for mid in &media_ids {
        let m: Option<Media> = sqlx::query_as(&format!("SELECT {MEDIA_COLS} FROM media WHERE id = ? AND org_id = ?"))
            .bind(mid)
            .bind(org)
            .fetch_optional(&state.db)
            .await?;
        if let Some(m) = m {
            media.insert(mid.clone(), m.to_json());
        }
    }
    Ok((json!(layouts), json!(playlists), json!(media)))
}

pub async fn build_manifest(state: &AppState, d: &Device) -> AppResult<Value> {
    let org = d.org_id.as_deref().unwrap_or_default();
    let schedules: Vec<Schedule> = sqlx::query_as(&format!(
        "SELECT {SCHED_COLS} FROM schedules
          WHERE org_id = ? AND enabled = 1
            AND (target_type = 'all'
                 OR (target_type = 'group' AND target_id = ?)
                 OR (target_type = 'display' AND target_id = ?))
          ORDER BY priority DESC, created_at"
    ))
    .bind(org)
    .bind(&d.group_id)
    .bind(&d.id)
    .fetch_all(&state.db)
    .await?;
    let trigger_layouts: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT layout_id FROM triggers WHERE org_id = ?
            AND (target_type = 'all' OR (target_type = 'group' AND target_id = ?) OR (target_type = 'display' AND target_id = ?))",
    )
    .bind(org)
    .bind(&d.group_id)
    .bind(&d.id)
    .fetch_all(&state.db)
    .await?;

    let mut ids: BTreeSet<String> = schedules.iter().map(|s| s.layout_id.clone()).collect();
    ids.extend(trigger_layouts);
    ids.extend(d.default_layout_id.clone());
    let (layouts, playlists, media) = bundle(state, org, &ids).await?;

    let alerts = crate::alerts::active_for_display(state, org, &d.id, d.group_id.as_deref()).await?;
    let body = json!({
        "display": { "id": d.id, "name": d.name },
        "alerts": alerts,
        "default_layout_id": d.default_layout_id,
        "schedules": schedules,
        "layouts": layouts,
        "playlists": playlists,
        "media": media,
    });
    let version = hex::encode(&Sha256::digest(body.to_string().as_bytes())[..12]);
    let mut out = body;
    out["status"] = json!("active");
    out["version"] = json!(version);
    out["server_time"] = json!(now());
    Ok(out)
}

async fn manifest(State(state): State<AppState>, d: Device) -> AppResult<Json<Value>> {
    sqlx::query("UPDATE displays SET last_seen = ? WHERE id = ?").bind(now()).bind(&d.id).execute(&state.db).await?;
    let suspended: bool = match &d.org_id {
        Some(o) => sqlx::query_scalar("SELECT suspended FROM organizations WHERE id = ?").bind(o).fetch_one(&state.db).await?,
        None => false,
    };
    if suspended {
        return Ok(Json(json!({ "status": "disabled" })));
    }
    match d.status.as_str() {
        "pending" => Ok(Json(json!({ "status": "pending", "pairing_code": d.pairing_code }))),
        "disabled" => Ok(Json(json!({ "status": "disabled" }))),
        _ => Ok(Json(build_manifest(&state, &d).await?)),
    }
}

async fn preview_layout(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    crate::content::fetch_layout(&state, user.org(), &id).await?;
    let ids = BTreeSet::from([id.clone()]);
    let (layouts, playlists, media) = bundle(&state, user.org(), &ids).await?;
    Ok(Json(json!({
        "status": "active", "version": "preview", "display": { "id": "preview", "name": "Preview" },
        "default_layout_id": id, "schedules": [], "layouts": layouts, "playlists": playlists, "media": media,
    })))
}

#[derive(Deserialize)]
struct PopRecord {
    layout_id: Option<String>,
    media_id: Option<String>,
    item_type: String,
    started_at: String,
    duration_ms: i64,
}

async fn proof_of_play(State(state): State<AppState>, d: Device, Json(records): Json<Vec<PopRecord>>) -> AppResult<Json<Value>> {
    let Some(org) = d.org_id.as_deref() else { return Err(AppError::Forbidden("display not paired".into())) };
    if records.len() > 5000 {
        return Err(AppError::BadRequest("batch too large".into()));
    }
    let mut tx = state.db.begin().await?;
    for r in &records {
        let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&r.started_at) else { continue };
        if r.duration_ms < 0 {
            continue;
        }
        let ts = ts.with_timezone(&chrono::Utc).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        sqlx::query(
            "INSERT INTO proof_of_play (org_id, display_id, layout_id, media_id, item_type, started_at, duration_ms)
             VALUES (?,?,?,?,?,?,?)",
        )
        .bind(org)
        .bind(&d.id)
        .bind(&r.layout_id)
        .bind(&r.media_id)
        .bind(r.item_type.chars().take(32).collect::<String>())
        .bind(&ts)
        .bind(r.duration_ms.min(86_400_000))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Json(json!({ "accepted": records.len() })))
}

// ---------- push socket ----------

pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>, d: Device) -> Response {
    ws.on_upgrade(move |socket| player_socket(socket, state, d))
}

async fn set_presence(state: &AppState, d: &Device, delta: i64) {
    let online = {
        let mut p = state.presence.write().await;
        let n = p.entry(d.id.clone()).or_insert(0);
        *n = (*n as i64 + delta).max(0) as usize;
        *n > 0
    };
    if let Some(org) = &d.org_id {
        let _ = sqlx::query("UPDATE displays SET last_seen = ? WHERE id = ?").bind(now()).bind(&d.id).execute(&state.db).await;
        state.publish(Event::to_admins(org, json!({ "type": "display_presence", "display_id": d.id, "online": online, "at": now() })));
    }
}

async fn player_socket(mut socket: WebSocket, state: AppState, d: Device) {
    let org = d.org_id.clone().unwrap_or_default();
    let mut rx = state.bus.subscribe();
    set_presence(&state, &d, 1).await;
    let _ = socket.send(Message::Text(json!({ "type": "hello", "status": d.status }).to_string().into())).await;
    let mut ping = tokio::time::interval(std::time::Duration::from_secs(25));
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) if ev.is_for_player(&org, &d.id) => {
                    if socket.send(Message::Text(ev.payload.to_string().into())).await.is_err() { break; }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // We may have missed something: make the player re-sync.
                    let _ = socket.send(Message::Text(json!({ "type": "refresh" }).to_string().into())).await;
                }
                Err(_) => break,
            },
            _ = ping.tick() => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(t))) => {
                    // Player status report: { type: "status", layout_id, version, info }
                    if let (Ok(v), Some(o)) = (serde_json::from_str::<Value>(&t), &d.org_id) {
                        if v["type"] == "status" {
                            if v["info"].is_object() {
                                let _ = sqlx::query("UPDATE displays SET player_info = ?, last_seen = ? WHERE id = ?")
                                    .bind(v["info"].to_string()).bind(now()).bind(&d.id).execute(&state.db).await;
                            }
                            state.publish(Event::to_admins(o, json!({
                                "type": "display_status", "display_id": d.id,
                                "layout_id": v["layout_id"], "version": v["version"], "at": now(),
                            })));
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    }
    set_presence(&state, &d, -1).await;
}
