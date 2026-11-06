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
        let mut head: Vec<u8> = Vec::with_capacity(8192);
        while let Some(chunk) = field.chunk().await.map_err(|e| AppError::BadRequest(e.to_string()))? {
            size += chunk.len();
            hasher.update(&chunk);
            if head.len() < 8192 {
                head.extend_from_slice(&chunk[..chunk.len().min(8192 - head.len())]);
            }
            f.write_all(&chunk).await?;
        }
        f.flush().await?;
        drop(f);
        if size == 0 {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(AppError::BadRequest("empty file".into()));
        }
        // Trust the bytes, not the file name or the browser's claimed type.
        let mime = match sniff(&head, &mime, &orig) {
            Ok(m) => m,
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e);
            }
        };
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


/// Decide the real type of an upload from its first bytes. Returns the MIME type to
/// store, or an error for executables, archives, unknown binaries and mismatches.
pub fn sniff(head: &[u8], claimed: &str, name: &str) -> AppResult<String> {
    const ALLOWED_PREFIX: &[&str] = &["image/", "video/", "audio/"];
    if let Some(t) = infer::get(head).filter(|t| !t.mime_type().starts_with("text/") && !t.mime_type().contains("xml")) {
        let m = t.mime_type();
        let ok = ALLOWED_PREFIX.iter().any(|p| m.starts_with(p)) || m == "application/pdf";
        if !ok {
            return Err(AppError::BadRequest(format!("this kind of file ({m}) can't be uploaded — use images, video, audio, PDF or HTML")));
        }
        let claimed_kind = claimed.split('/').next().unwrap_or("");
        let real_kind = m.split('/').next().unwrap_or("");
        if ALLOWED_PREFIX.iter().any(|p| claimed.starts_with(p)) && claimed_kind != real_kind {
            return Err(AppError::BadRequest("the file's content doesn't match its type".into()));
        }
        return Ok(m.to_string());
    }
    // Not a known binary: only text formats we support (HTML, SVG) are allowed.
    let text = !head.contains(&0) && std::str::from_utf8(&head[..head.len().saturating_sub(4)]).is_ok();
    let lower = name.to_ascii_lowercase();
    let start = String::from_utf8_lossy(&head[..head.len().min(1024)]).to_ascii_lowercase();
    if text && (lower.ends_with(".svg") || start.contains("<svg")) {
        return Ok("image/svg+xml".into());
    }
    if text && (lower.ends_with(".html") || lower.ends_with(".htm") || start.contains("<html") || start.contains("<!doctype html")) {
        return Ok("text/html".into());
    }
    Err(AppError::BadRequest("unsupported file type — use images, video, audio, PDF or HTML".into()))
}

#[cfg(test)]
mod sniff_tests {
    use super::sniff;
    #[test]
    fn types() {
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        assert_eq!(sniff(&png, "image/png", "a.png").unwrap(), "image/png");
        assert_eq!(sniff(&png, "application/octet-stream", "a.jpg").unwrap(), "image/png", "real type wins");
        assert!(sniff(b"MZ\x90\x00\x03\x00\x00\x00\x04\x00\x00\x00\xff\xff", "image/png", "evil.png").is_err(), "Windows executable");
        assert!(sniff(b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00", "video/mp4", "x.mp4").is_err(), "Linux executable");
        assert!(sniff(b"PK\x03\x04\x14\x00\x00\x00\x08\x00", "image/png", "a.png").is_err(), "zip archive");
        assert!(sniff(&png, "video/mp4", "a.mp4").is_err(), "content/type mismatch");
        assert_eq!(sniff(b"<svg xmlns='http://www.w3.org/2000/svg'></svg>", "image/svg+xml", "a.svg").unwrap(), "image/svg+xml");
        assert_eq!(sniff(b"<!doctype html><html><body>hi</body></html>", "text/html", "a.html").unwrap(), "text/html");
        assert!(sniff(b"just some text", "text/plain", "notes.txt").is_err());
    }
}
