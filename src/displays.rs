use axum::{
    extract::{Path, State},
    routing::{get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    auth::{audit, AuthUser},
    error::{AppError, AppResult},
    hub::{is_online, Event},
    state::AppState,
    util::new_id,
};

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Display {
    pub id: String,
    pub name: String,
    pub status: String,
    pub group_id: Option<String>,
    pub default_layout_id: Option<String>,
    pub player_info: String,
    pub last_seen: Option<String>,
    pub created_at: String,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/displays", get(list))
        .route("/displays/claim", post(claim))
        .route("/displays/{id}", patch(update).delete(remove))
        .route("/displays/{id}/command", post(command))
        .route("/groups", get(list_groups).post(create_group))
        .route("/groups/{id}", patch(rename_group).delete(delete_group))
}

/// Resolve a schedule/trigger target into concrete display ids.
pub async fn resolve_targets(state: &AppState, org: &str, target_type: &str, target_id: Option<&str>) -> AppResult<Vec<String>> {
    let ids: Vec<String> = match target_type {
        "all" => sqlx::query_scalar("SELECT id FROM displays WHERE org_id = ? AND status = 'active'")
            .bind(org)
            .fetch_all(&state.db)
            .await?,
        "group" => sqlx::query_scalar("SELECT id FROM displays WHERE org_id = ? AND group_id = ? AND status = 'active'")
            .bind(org)
            .bind(target_id)
            .fetch_all(&state.db)
            .await?,
        _ => sqlx::query_scalar("SELECT id FROM displays WHERE org_id = ? AND id = ? AND status = 'active'")
            .bind(org)
            .bind(target_id)
            .fetch_all(&state.db)
            .await?,
    };
    Ok(ids)
}

pub async fn validate_target(state: &AppState, org: &str, target_type: &str, target_id: Option<&str>) -> AppResult<()> {
    let n: i64 = match target_type {
        "all" => return Ok(()),
        "group" => sqlx::query_scalar("SELECT COUNT(*) FROM display_groups WHERE id = ? AND org_id = ?"),
        "display" => sqlx::query_scalar("SELECT COUNT(*) FROM displays WHERE id = ? AND org_id = ?"),
        _ => return Err(AppError::BadRequest("target_type must be all, group or display".into())),
    }
    .bind(target_id)
    .bind(org)
    .fetch_one(&state.db)
    .await?;
    if n == 0 {
        return Err(AppError::BadRequest("target not found".into()));
    }
    Ok(())
}

async fn list(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Vec<Value>>> {
    let rows = sqlx::query_as::<_, Display>(
        "SELECT id, name, status, group_id, default_layout_id, player_info, last_seen, created_at
           FROM displays WHERE org_id = ? ORDER BY name",
    )
    .bind(user.org())
    .fetch_all(&state.db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for d in rows {
        let online = is_online(&state, &d.id).await;
        out.push(json!({
            "id": d.id, "name": d.name, "status": d.status, "group_id": d.group_id,
            "default_layout_id": d.default_layout_id, "last_seen": d.last_seen, "created_at": d.created_at,
            "player_info": serde_json::from_str::<Value>(&d.player_info).unwrap_or(json!({})),
            "online": online,
        }));
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct ClaimReq {
    code: String,
    name: String,
    group_id: Option<String>,
    default_layout_id: Option<String>,
}

async fn claim(State(state): State<AppState>, user: AuthUser, Json(req): Json<ClaimReq>) -> AppResult<Json<Value>> {
    user.require_admin()?;
    let code = req.code.trim().to_uppercase();
    let id: Option<String> = sqlx::query_scalar("SELECT id FROM displays WHERE pairing_code = ? AND org_id IS NULL")
        .bind(&code)
        .fetch_optional(&state.db)
        .await?;
    let id = id.ok_or_else(|| AppError::BadRequest("no screen is waiting with that code".into()))?;
    let (max, used): (Option<i64>, i64) = sqlx::query_as(
        "SELECT o.max_screens, (SELECT COUNT(*) FROM displays d WHERE d.org_id = o.id) FROM organizations o WHERE o.id = ?",
    )
    .bind(user.org())
    .fetch_one(&state.db)
    .await?;
    if let Some(max) = max {
        if used >= max {
            return Err(AppError::Forbidden(format!(
                "your plan allows {max} screen(s) and all are in use — remove a screen or upgrade your plan"
            )));
        }
    }
    let name = if req.name.trim().is_empty() { format!("Screen {code}") } else { req.name.trim().to_string() };
    let group = req.group_id.filter(|s| !s.is_empty());
    let layout = req.default_layout_id.filter(|s| !s.is_empty());
    if let Some(g) = &group {
        validate_target(&state, user.org(), "group", Some(g)).await?;
    }
    if let Some(l) = &layout {
        crate::content::fetch_layout(&state, user.org(), l).await?;
    }
    sqlx::query(
        "UPDATE displays SET org_id = ?, name = ?, status = 'active', pairing_code = NULL, group_id = ?, default_layout_id = ?
          WHERE id = ?",
    )
    .bind(user.org())
    .bind(&name)
    .bind(group)
    .bind(layout)
    .bind(&id)
    .execute(&state.db)
    .await?;
    // The unpaired player is listening on its socket under the "unclaimed" org.
    state.publish(Event::to_displays("", vec![id.clone()], json!({ "type": "claimed" })));
    audit(&state, user.org(), Some(&user.0.id), "display.claimed", &name).await;
    Ok(Json(json!({ "id": id, "name": name })))
}

#[derive(Deserialize)]
struct UpdateReq {
    name: Option<String>,
    status: Option<String>,
    group_id: Option<Option<String>>,
    default_layout_id: Option<Option<String>>,
}

async fn update(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>, Json(req): Json<UpdateReq>) -> AppResult<Json<Value>> {
    user.require_admin()?;
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM displays WHERE id = ? AND org_id = ?")
        .bind(&id)
        .bind(user.org())
        .fetch_one(&state.db)
        .await?;
    if exists == 0 {
        return Err(AppError::NotFound);
    }
    if let Some(n) = req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE displays SET name = ? WHERE id = ?").bind(n).bind(&id).execute(&state.db).await?;
    }
    if let Some(s) = &req.status {
        if s != "active" && s != "disabled" {
            return Err(AppError::BadRequest("status must be active or disabled".into()));
        }
        sqlx::query("UPDATE displays SET status = ? WHERE id = ?").bind(s).bind(&id).execute(&state.db).await?;
    }
    if let Some(g) = req.group_id {
        let g = g.filter(|s| !s.is_empty());
        if let Some(gid) = &g {
            validate_target(&state, user.org(), "group", Some(gid)).await?;
        }
        sqlx::query("UPDATE displays SET group_id = ? WHERE id = ?").bind(g).bind(&id).execute(&state.db).await?;
    }
    if let Some(l) = req.default_layout_id {
        let l = l.filter(|s| !s.is_empty());
        if let Some(lid) = &l {
            crate::content::fetch_layout(&state, user.org(), lid).await?;
        }
        sqlx::query("UPDATE displays SET default_layout_id = ? WHERE id = ?").bind(l).bind(&id).execute(&state.db).await?;
    }
    state.publish(Event::to_displays(user.org(), vec![id.clone()], json!({ "type": "refresh" })));
    Ok(Json(json!({ "ok": true })))
}

async fn remove(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    user.require_admin()?;
    state.publish(Event::to_displays(user.org(), vec![id.clone()], json!({ "type": "unpaired" })));
    let res = sqlx::query("DELETE FROM displays WHERE id = ? AND org_id = ?")
        .bind(&id)
        .bind(user.org())
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    audit(&state, user.org(), Some(&user.0.id), "display.deleted", &id).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct CommandReq {
    command: String,
}

async fn command(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>, Json(req): Json<CommandReq>) -> AppResult<Json<Value>> {
    user.require_editor()?;
    if !["reload", "identify", "clear_cache", "refresh"].contains(&req.command.as_str()) {
        return Err(AppError::BadRequest("unknown command".into()));
    }
    let ids = resolve_targets(&state, user.org(), "display", Some(&id)).await?;
    if ids.is_empty() {
        return Err(AppError::NotFound);
    }
    let online = is_online(&state, &id).await;
    state.publish(Event::to_displays(user.org(), ids, json!({ "type": "command", "command": req.command })));
    Ok(Json(json!({ "ok": true, "delivered": online })))
}

// ---------- groups ----------

#[derive(Serialize, sqlx::FromRow)]
struct Group {
    id: String,
    name: String,
    display_count: i64,
}

async fn list_groups(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Vec<Group>>> {
    let rows = sqlx::query_as::<_, Group>(
        "SELECT g.id, g.name, (SELECT COUNT(*) FROM displays d WHERE d.group_id = g.id) AS display_count
           FROM display_groups g WHERE g.org_id = ? ORDER BY g.name",
    )
    .bind(user.org())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
struct GroupReq {
    name: String,
}

async fn create_group(State(state): State<AppState>, user: AuthUser, Json(req): Json<GroupReq>) -> AppResult<Json<Value>> {
    user.require_admin()?;
    if req.name.trim().is_empty() {
        return Err(AppError::BadRequest("name required".into()));
    }
    let id = new_id();
    sqlx::query("INSERT INTO display_groups (id, org_id, name) VALUES (?,?,?)")
        .bind(&id)
        .bind(user.org())
        .bind(req.name.trim())
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "id": id, "name": req.name.trim(), "display_count": 0 })))
}

async fn rename_group(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>, Json(req): Json<GroupReq>) -> AppResult<Json<Value>> {
    user.require_admin()?;
    let r = sqlx::query("UPDATE display_groups SET name = ? WHERE id = ? AND org_id = ?")
        .bind(req.name.trim())
        .bind(&id)
        .bind(user.org())
        .execute(&state.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}

async fn delete_group(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    user.require_admin()?;
    let r = sqlx::query("DELETE FROM display_groups WHERE id = ? AND org_id = ?")
        .bind(&id)
        .bind(user.org())
        .execute(&state.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    sqlx::query("DELETE FROM schedules WHERE target_type = 'group' AND target_id = ?").bind(&id).execute(&state.db).await?;
    state.content_changed(user.org());
    Ok(Json(json!({ "ok": true })))
}
