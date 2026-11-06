//! Platform administration for the SaaS owner: every customer organization, their
//! plan and limits, AI access, suspension, and platform-wide usage.

use axum::{
    extract::{Path, State},
    routing::{get, patch},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    auth::{audit, hash_password, AuthUser},
    error::{AppError, AppResult},
    hub::is_online,
    state::AppState,
    util::{new_id, now},
};

pub const PLANS: &[&str] = &["trial", "pilot", "cloud", "education", "onprem", "internal"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/platform/overview", get(overview))
        .route("/platform/orgs", get(list_orgs).post(create_org))
        .route("/platform/orgs/{id}", patch(update_org).delete(delete_org))
        .route("/platform/ai-usage", get(ai_usage))
}

pub fn month_start() -> String {
    use chrono::Datelike;
    let n = chrono::Utc::now();
    format!("{:04}-{:02}-01T00:00:00Z", n.year(), n.month())
}

async fn overview(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    user.require_platform()?;
    let q = |sql: &'static str| {
        let db = state.db.clone();
        async move { sqlx::query_scalar::<_, i64>(sql).fetch_one(&db).await }
    };
    let online = state.presence.read().await.values().filter(|n| **n > 0).count();
    let ai_month: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ai_usage WHERE ok = 1 AND created_at >= ?")
        .bind(month_start())
        .fetch_one(&state.db)
        .await?;
    let tokens: (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0) FROM ai_usage WHERE created_at >= ?",
    )
    .bind(month_start())
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({
        "organizations": q("SELECT COUNT(*) FROM organizations").await?,
        "suspended": q("SELECT COUNT(*) FROM organizations WHERE suspended = 1").await?,
        "users": q("SELECT COUNT(*) FROM users").await?,
        "displays": q("SELECT COUNT(*) FROM displays WHERE org_id IS NOT NULL").await?,
        "online": online,
        "storage_bytes": q("SELECT COALESCE(SUM(size),0) FROM media").await?,
        "plays_24h": sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM proof_of_play WHERE started_at >= ?")
            .bind((chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .fetch_one(&state.db).await?,
        "ai_generations_month": ai_month,
        "ai_input_tokens_month": tokens.0,
        "ai_output_tokens_month": tokens.1,
        "ai_configured": crate::ai::configured(),
    })))
}

#[derive(Serialize, sqlx::FromRow)]
struct OrgRow {
    id: String,
    name: String,
    plan: String,
    max_screens: Option<i64>,
    ai_enabled: bool,
    ai_monthly_limit: i64,
    suspended: bool,
    notes: String,
    created_at: String,
    users: i64,
    displays: i64,
    storage_bytes: i64,
    ai_used_month: i64,
    owner_email: Option<String>,
    last_seen: Option<String>,
}

async fn list_orgs(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Vec<Value>>> {
    user.require_platform()?;
    let rows = sqlx::query_as::<_, OrgRow>(
        "SELECT o.id, o.name, o.plan, o.max_screens, o.ai_enabled, o.ai_monthly_limit, o.suspended, o.notes, o.created_at,
                (SELECT COUNT(*) FROM users u WHERE u.org_id = o.id) AS users,
                (SELECT COUNT(*) FROM displays d WHERE d.org_id = o.id) AS displays,
                (SELECT COALESCE(SUM(size),0) FROM media m WHERE m.org_id = o.id) AS storage_bytes,
                (SELECT COUNT(*) FROM ai_usage a WHERE a.org_id = o.id AND a.ok = 1 AND a.created_at >= ?) AS ai_used_month,
                (SELECT email FROM users u WHERE u.org_id = o.id AND u.role = 'owner' ORDER BY created_at LIMIT 1) AS owner_email,
                (SELECT MAX(last_seen) FROM displays d WHERE d.org_id = o.id) AS last_seen
           FROM organizations o ORDER BY o.created_at DESC",
    )
    .bind(month_start())
    .fetch_all(&state.db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM displays WHERE org_id = ?")
            .bind(&r.id)
            .fetch_all(&state.db)
            .await?;
        let mut online = 0;
        for i in &ids {
            if is_online(&state, i).await {
                online += 1;
            }
        }
        let mut v = serde_json::to_value(&r)?;
        v["online"] = json!(online);
        v["is_own"] = json!(r.id == user.org());
        out.push(v);
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct CreateOrgReq {
    org_name: String,
    owner_name: String,
    owner_email: String,
    owner_password: String,
    #[serde(default = "trial")]
    plan: String,
    max_screens: Option<i64>,
    #[serde(default)]
    notes: String,
}
fn trial() -> String { "trial".into() }

fn check_plan(p: &str) -> AppResult<()> {
    if PLANS.contains(&p) { Ok(()) } else { Err(AppError::BadRequest(format!("plan must be one of {}", PLANS.join(", ")))) }
}

async fn create_org(State(state): State<AppState>, user: AuthUser, Json(r): Json<CreateOrgReq>) -> AppResult<Json<Value>> {
    user.require_platform()?;
    check_plan(&r.plan)?;
    let email = r.owner_email.trim().to_lowercase();
    if r.org_name.trim().is_empty() || r.owner_name.trim().is_empty() || !email.contains('@') {
        return Err(AppError::BadRequest("organization, owner name and a valid email are required".into()));
    }
    if r.owner_password.len() < 8 {
        return Err(AppError::BadRequest("password must be at least 8 characters".into()));
    }
    if let Some(m) = r.max_screens {
        if m < 0 { return Err(AppError::BadRequest("max screens must be 0 or more".into())); }
    }
    let org_id = new_id();
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO organizations (id, name, created_at, plan, max_screens, notes) VALUES (?,?,?,?,?,?)")
        .bind(&org_id).bind(r.org_name.trim()).bind(now()).bind(&r.plan).bind(r.max_screens).bind(r.notes.trim())
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO users (id, org_id, email, name, password_hash, role, created_at) VALUES (?,?,?,?,?,'owner',?)")
        .bind(new_id()).bind(&org_id).bind(&email).bind(r.owner_name.trim()).bind(hash_password(&r.owner_password)?).bind(now())
        .execute(&mut *tx).await?;
    tx.commit().await?;
    audit(&state, user.org(), Some(&user.0.id), "platform.org_created", r.org_name.trim()).await;
    Ok(Json(json!({ "id": org_id, "name": r.org_name.trim() })))
}

#[derive(Deserialize)]
struct UpdateOrgReq {
    name: Option<String>,
    plan: Option<String>,
    max_screens: Option<Option<i64>>,
    ai_enabled: Option<bool>,
    ai_monthly_limit: Option<i64>,
    suspended: Option<bool>,
    notes: Option<String>,
}

async fn update_org(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>, Json(r): Json<UpdateOrgReq>) -> AppResult<Json<Value>> {
    user.require_platform()?;
    let name: String = sqlx::query_scalar("SELECT name FROM organizations WHERE id = ?")
        .bind(&id).fetch_optional(&state.db).await?.ok_or(AppError::NotFound)?;
    if let Some(n) = r.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE organizations SET name = ? WHERE id = ?").bind(n).bind(&id).execute(&state.db).await?;
    }
    if let Some(p) = &r.plan {
        check_plan(p)?;
        sqlx::query("UPDATE organizations SET plan = ? WHERE id = ?").bind(p).bind(&id).execute(&state.db).await?;
    }
    if let Some(m) = r.max_screens {
        if m.map(|v| v < 0).unwrap_or(false) { return Err(AppError::BadRequest("max screens must be 0 or more".into())); }
        sqlx::query("UPDATE organizations SET max_screens = ? WHERE id = ?").bind(m).bind(&id).execute(&state.db).await?;
    }
    if let Some(a) = r.ai_enabled {
        sqlx::query("UPDATE organizations SET ai_enabled = ? WHERE id = ?").bind(a).bind(&id).execute(&state.db).await?;
    }
    if let Some(l) = r.ai_monthly_limit {
        if !(0..=100_000).contains(&l) { return Err(AppError::BadRequest("AI limit must be 0–100000".into())); }
        sqlx::query("UPDATE organizations SET ai_monthly_limit = ? WHERE id = ?").bind(l).bind(&id).execute(&state.db).await?;
    }
    if let Some(s) = r.suspended {
        if s && id == user.org() {
            return Err(AppError::BadRequest("you cannot suspend your own organization".into()));
        }
        sqlx::query("UPDATE organizations SET suspended = ? WHERE id = ?").bind(s).bind(&id).execute(&state.db).await?;
        state.content_changed(&id); // screens re-check and show / leave the disabled screen
        audit(&state, user.org(), Some(&user.0.id), if s { "platform.org_suspended" } else { "platform.org_reactivated" }, &name).await;
    }
    if let Some(n) = &r.notes {
        sqlx::query("UPDATE organizations SET notes = ? WHERE id = ?").bind(n.trim()).bind(&id).execute(&state.db).await?;
    }
    Ok(Json(json!({ "ok": true })))
}

async fn delete_org(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    user.require_platform()?;
    if id == user.org() {
        return Err(AppError::BadRequest("you cannot delete your own organization".into()));
    }
    let name: String = sqlx::query_scalar("SELECT name FROM organizations WHERE id = ?")
        .bind(&id).fetch_optional(&state.db).await?.ok_or(AppError::NotFound)?;
    let files: Vec<String> = sqlx::query_scalar("SELECT DISTINCT file_name FROM media WHERE org_id = ?")
        .bind(&id).fetch_all(&state.db).await?;
    let displays: Vec<String> = sqlx::query_scalar("SELECT id FROM displays WHERE org_id = ?")
        .bind(&id).fetch_all(&state.db).await?;
    state.publish(crate::hub::Event::to_displays(&id, displays, json!({ "type": "unpaired" })));
    sqlx::query("DELETE FROM organizations WHERE id = ?").bind(&id).execute(&state.db).await?;
    sqlx::query("DELETE FROM proof_of_play WHERE org_id = ?").bind(&id).execute(&state.db).await?;
    sqlx::query("DELETE FROM audit_log WHERE org_id = ?").bind(&id).execute(&state.db).await?;
    for f in files {
        let still: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media WHERE file_name = ?").bind(&f).fetch_one(&state.db).await?;
        if still == 0 {
            let _ = tokio::fs::remove_file(state.data_dir.join("media").join(&f)).await;
        }
    }
    audit(&state, user.org(), Some(&user.0.id), "platform.org_deleted", &name).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Serialize, sqlx::FromRow)]
struct UsageRow {
    id: i64,
    org_name: Option<String>,
    kind: String,
    prompt: String,
    input_tokens: i64,
    output_tokens: i64,
    ok: bool,
    error: Option<String>,
    created_at: String,
}

async fn ai_usage(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Vec<UsageRow>>> {
    user.require_platform()?;
    let rows = sqlx::query_as::<_, UsageRow>(
        "SELECT a.id, o.name AS org_name, a.kind, a.prompt, a.input_tokens, a.output_tokens, a.ok, a.error, a.created_at
           FROM ai_usage a LEFT JOIN organizations o ON o.id = a.org_id ORDER BY a.id DESC LIMIT 100",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}
