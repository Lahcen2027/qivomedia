use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    routing::{get, patch},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::{
    auth::{audit, AuthUser},
    error::{AppError, AppResult},
    state::AppState,
    util::{new_id, now},
};

const MAX_UPLOAD: usize = 4 * 1024 * 1024 * 1024; // 4 GiB

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Media {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub mime: String,
    pub size: i64,
    pub sha256: String,
    pub file_name: String,
    pub duration_ms: Option<i64>,
    pub tags: String,
    pub created_at: String,
}

impl Media {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "name": self.name, "kind": self.kind, "mime": self.mime,
            "size": self.size, "sha256": self.sha256, "url": format!("/files/{}", self.file_name),
            "duration_ms": self.duration_ms,
            "tags": serde_json::from_str::<Value>(&self.tags).unwrap_or(json!([])),
            "created_at": self.created_at,
        })
    }
}

pub const MEDIA_COLS: &str = "id, name, kind, mime, size, sha256, file_name, duration_ms, tags, created_at";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/media", get(list).post(upload).layer(DefaultBodyLimit::max(MAX_UPLOAD)))
        .route("/media/{id}", patch(update).delete(remove))
}

fn kind_for(mime: &str) -> &'static str {
    match mime.split('/').next().unwrap_or("") {
        "image" => "image",
        "video" => "video",
        "audio" => "audio",
        _ if mime == "application/pdf" => "pdf",
        _ if mime == "text/html" => "html",
        _ => "other",
    }
}

fn safe_ext(name: &str) -> String {
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .filter(|e| e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|e| format!(".{e}"))
        .unwrap_or_default()
}

async fn list(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Vec<Value>>> {
    let rows = sqlx::query_as::<_, Media>(&format!(
        "SELECT {MEDIA_COLS} FROM media WHERE org_id = ? ORDER BY created_at DESC"
    ))
    .bind(user.org())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.iter().map(Media::to_json).collect()))
}

async fn upload(State(state): State<AppState>, user: AuthUser, mut mp: Multipart) -> AppResult<Json<Vec<Value>>> {
    user.require_editor()?;
    let mut created = Vec::new();
    let mut duration_ms: Option<i64> = None;

    while let Some(mut field) = mp.next_field().await.map_err(|e| AppError::BadRequest(e.to_string()))? {
        match field.name() {
            Some("duration_ms") => {
                duration_ms = field.text().await.ok().and_then(|t| t.parse().ok());
                continue;
            }
            Some("file") => {}
            _ => continue,
        }
        let orig = field.file_name().unwrap_or("upload").to_string();
        let mime = field
            .content_type()
            .map(str::to_owned)
            .filter(|m| m != "application/octet-stream")
            .unwrap_or_else(|| mime_guess::from_path(&orig).first_or_octet_stream().to_string());

        let tmp = state.data_dir.join("tmp").join(new_id());
        let mut f = tokio::fs::File::create(&tmp).await?;
        let mut hasher = Sha256::new();
        let mut size: usize = 0;
        while let Some(chunk) = field.chunk().await.map_err(|e| AppError::BadRequest(e.to_string()))? {
            size += chunk.len();
            hasher.update(&chunk);
            f.write_all(&chunk).await?;
        }
        f.flush().await?;
        drop(f);
        if size == 0 {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(AppError::BadRequest("empty file".into()));
        }
        let sha = hex::encode(hasher.finalize());
        let file_name = format!("{sha}{}", safe_ext(&orig));
        let dest = state.data_dir.join("media").join(&file_name);
        if tokio::fs::try_exists(&dest).await.unwrap_or(false) {
            tokio::fs::remove_file(&tmp).await?; // content-addressed dedupe
        } else {
            tokio::fs::rename(&tmp, &dest).await?;
        }

        let id = new_id();
        sqlx::query(
            "INSERT INTO media (id, org_id, name, kind, mime, size, sha256, file_name, duration_ms, created_at)
             VALUES (?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&id)
        .bind(user.org())
        .bind(&orig)
        .bind(kind_for(&mime))
        .bind(&mime)
        .bind(size as i64)
        .bind(&sha)
        .bind(&file_name)
        .bind(duration_ms)
        .bind(now())
        .execute(&state.db)
        .await?;
        audit(&state, user.org(), Some(&user.0.id), "media.uploaded", &orig).await;
        let m = sqlx::query_as::<_, Media>(&format!("SELECT {MEDIA_COLS} FROM media WHERE id = ?"))
            .bind(&id)
            .fetch_one(&state.db)
            .await?;
        created.push(m.to_json());
    }
    if created.is_empty() {
        return Err(AppError::BadRequest("no file field in upload".into()));
    }
    Ok(Json(created))
}

#[derive(Deserialize)]
struct UpdateReq {
    name: Option<String>,
    tags: Option<Vec<String>>,
    duration_ms: Option<i64>,
}

async fn update(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<UpdateReq>,
) -> AppResult<Json<Value>> {
    user.require_editor()?;
    let m = sqlx::query_as::<_, Media>(&format!("SELECT {MEDIA_COLS} FROM media WHERE id = ? AND org_id = ?"))
        .bind(&id)
        .bind(user.org())
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let name = req.name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).unwrap_or(m.name);
    let tags = req.tags.map(|t| serde_json::to_string(&t).unwrap()).unwrap_or(m.tags);
    let duration = req.duration_ms.or(m.duration_ms);
    sqlx::query("UPDATE media SET name = ?, tags = ?, duration_ms = ? WHERE id = ?")
        .bind(&name)
        .bind(&tags)
        .bind(duration)
        .bind(&id)
        .execute(&state.db)
        .await?;
    state.content_changed(user.org());
    let m = sqlx::query_as::<_, Media>(&format!("SELECT {MEDIA_COLS} FROM media WHERE id = ?"))
        .bind(&id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(m.to_json()))
}

async fn remove(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    user.require_editor()?;
    let m = sqlx::query_as::<_, Media>(&format!("SELECT {MEDIA_COLS} FROM media WHERE id = ? AND org_id = ?"))
        .bind(&id)
        .bind(user.org())
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let in_use: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlists WHERE org_id = ? AND items LIKE ?")
        .bind(user.org())
        .bind(format!("%{id}%"))
        .fetch_one(&state.db)
        .await?;
    if in_use > 0 {
        return Err(AppError::Conflict(format!("media is used in {in_use} playlist(s); remove it there first")));
    }
    sqlx::query("DELETE FROM media WHERE id = ?").bind(&id).execute(&state.db).await?;
    let still_referenced: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media WHERE file_name = ?")
        .bind(&m.file_name)
        .fetch_one(&state.db)
        .await?;
    if still_referenced == 0 {
        let _ = tokio::fs::remove_file(state.data_dir.join("media").join(&m.file_name)).await;
    }
    audit(&state, user.org(), Some(&user.0.id), "media.deleted", &m.name).await;
    Ok(Json(json!({ "ok": true })))
}
