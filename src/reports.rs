use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{auth::AuthUser, error::AppResult, hub::is_online, state::AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/reports/pop", get(pop_report))
        .route("/reports/overview", get(overview))
}

#[derive(Deserialize)]
struct Range {
    from: Option<String>,
    to: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
struct MediaRow {
    media_id: Option<String>,
    name: Option<String>,
    item_type: String,
    plays: i64,
    seconds: i64,
}

#[derive(Serialize, sqlx::FromRow)]
struct DisplayRow {
    display_id: String,
    name: Option<String>,
    plays: i64,
    seconds: i64,
}

#[derive(Serialize, sqlx::FromRow)]
struct DayRow {
    day: String,
    plays: i64,
    seconds: i64,
}

async fn pop_report(State(state): State<AppState>, user: AuthUser, Query(r): Query<Range>) -> AppResult<Json<Value>> {
    let from = r.from.unwrap_or_else(|| (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    let to = r.to.unwrap_or_else(|| (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    let by_media = sqlx::query_as::<_, MediaRow>(
        "SELECT p.media_id, m.name, p.item_type, COUNT(*) AS plays, SUM(p.duration_ms)/1000 AS seconds
           FROM proof_of_play p LEFT JOIN media m ON m.id = p.media_id
          WHERE p.org_id = ? AND p.started_at >= ? AND p.started_at < ?
          GROUP BY p.media_id, p.item_type ORDER BY plays DESC LIMIT 100",
    )
    .bind(user.org()).bind(&from).bind(&to)
    .fetch_all(&state.db).await?;
    let by_display = sqlx::query_as::<_, DisplayRow>(
        "SELECT p.display_id, d.name, COUNT(*) AS plays, SUM(p.duration_ms)/1000 AS seconds
           FROM proof_of_play p LEFT JOIN displays d ON d.id = p.display_id
          WHERE p.org_id = ? AND p.started_at >= ? AND p.started_at < ?
          GROUP BY p.display_id ORDER BY plays DESC",
    )
    .bind(user.org()).bind(&from).bind(&to)
    .fetch_all(&state.db).await?;
    let by_day = sqlx::query_as::<_, DayRow>(
        "SELECT substr(started_at, 1, 10) AS day, COUNT(*) AS plays, SUM(duration_ms)/1000 AS seconds
           FROM proof_of_play WHERE org_id = ? AND started_at >= ? AND started_at < ?
          GROUP BY day ORDER BY day",
    )
    .bind(user.org()).bind(&from).bind(&to)
    .fetch_all(&state.db).await?;
    Ok(Json(json!({ "from": from, "to": to, "by_media": by_media, "by_display": by_display, "by_day": by_day })))
}

async fn overview(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    let org = user.org();
    let count = |sql: &'static str| {
        let db = state.db.clone();
        let org = org.to_string();
        async move { sqlx::query_scalar::<_, i64>(sql).bind(org).fetch_one(&db).await }
    };
    let displays: Vec<String> = sqlx::query_scalar("SELECT id FROM displays WHERE org_id = ? AND status = 'active'")
        .bind(org)
        .fetch_all(&state.db)
        .await?;
    let mut online = 0;
    for d in &displays {
        if is_online(&state, d).await {
            online += 1;
        }
    }
    let storage: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(size),0) FROM media WHERE org_id = ?")
        .bind(org)
        .fetch_one(&state.db)
        .await?;
    let plays_24h: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM proof_of_play WHERE org_id = ? AND started_at >= ?")
        .bind(org)
        .bind((chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .fetch_one(&state.db)
        .await?;
    Ok(Json(json!({
        "displays": displays.len(),
        "online": online,
        "media": count("SELECT COUNT(*) FROM media WHERE org_id = ?").await?,
        "playlists": count("SELECT COUNT(*) FROM playlists WHERE org_id = ?").await?,
        "layouts": count("SELECT COUNT(*) FROM layouts WHERE org_id = ?").await?,
        "schedules": count("SELECT COUNT(*) FROM schedules WHERE org_id = ? AND enabled = 1").await?,
        "storage_bytes": storage,
        "plays_24h": plays_24h,
    })))
}
