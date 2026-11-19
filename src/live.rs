//! Live video on screens: IP cameras, encoders and IPTV channels.
//!
//! * **Relayed** (RTSP, RTMP, SRT, UDP/multicast MPEG-TS, or HTTP(S) HLS): the server pulls
//!   the stream with ffmpeg and republishes it as HLS at
//!   `/api/live/<stream id>/<token>/index.m3u8`, which every screen can play. The relay starts
//!   when a screen asks for it and stops 45 s after the last screen stops watching. If the
//!   source drops, the relay restarts it by itself. Video is copied as it is when it's already
//!   H.264, and converted otherwise (H.265 cameras, MPEG-2 IPTV).
//! * **Direct** (HTTP(S) HLS only): screens fetch the stream themselves, which suits public
//!   channels that allow it (CORS).
//!
//! Security:
//! * Source addresses can contain camera passwords, so they are stored encrypted.
//! * Only streaming protocols are accepted, never `file:`, and ffmpeg runs with a protocol
//!   whitelist for that scheme.
//! * The server itself (loopback) and link-local addresses (cloud metadata) are refused. With
//!   `QIVO_STREAM_ALLOW_PRIVATE=0` (hosted SaaS), private networks are refused too.
//! * The screen address carries a token derived from the server key and the stream (HMAC), so
//!   it can't be guessed. Deleting a stream revokes it.

use std::{
    collections::HashMap,
    net::IpAddr,
    path::PathBuf,
    process::Stdio,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{io::AsyncBufReadExt, process::Command};

use crate::{
    auth::{audit, AuthUser},
    error::{AppError, AppResult},
    state::AppState,
    util::{new_id, now},
};

const IDLE_STOP: Duration = Duration::from_secs(45);
const START_WAIT: Duration = Duration::from_secs(20);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/streams", get(list).post(create))
        .route("/streams/{id}", put(update).delete(remove))
        .route("/streams/{id}/test", post(test))
        .route("/live/{id}/{token}/{file}", get(serve))
}

fn max_relays() -> usize {
    std::env::var("QIVO_MAX_RELAYS").ok().and_then(|v| v.parse().ok()).unwrap_or(16)
}

// ------------------------------------------------------------------ sources

#[derive(Debug, PartialEq)]
pub struct Source {
    pub url: String,
    pub scheme: String,
    /// The address without the password, for display.
    pub hint: String,
}

fn whitelist(scheme: &str) -> &'static str {
    match scheme {
        "rtsp" | "rtsps" => "rtsp,rtsps,rtp,srtp,udp,tcp,tls",
        "rtmp" | "rtmps" => "rtmp,rtmps,tcp,tls",
        "srt" => "srt,udp",
        "udp" | "rtp" => "udp,rtp",
        _ => "http,https,tcp,tls,crypto",
    }
}

fn blocked_ip(ip: IpAddr, allow_private: bool) -> Option<&'static str> {
    let loopback_ok = std::env::var("QIVO_STREAM_ALLOW_LOOPBACK").is_ok_and(|v| v == "1");
    match ip {
        IpAddr::V4(v) => {
            if v.is_loopback() && !loopback_ok {
                return Some("this server itself");
            }
            if v.is_unspecified() || v.is_broadcast() {
                return Some("an invalid address");
            }
            if v.is_link_local() {
                return Some("a link-local address (cloud metadata)");
            }
            let o = v.octets();
            let cgnat = o[0] == 100 && (64..128).contains(&o[1]);
            if !allow_private && (v.is_private() || cgnat) {
                return Some("a private network address");
            }
            None
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return blocked_ip(IpAddr::V4(v4), allow_private);
            }
            if v.is_loopback() && !loopback_ok {
                return Some("this server itself");
            }
            if v.is_unspecified() {
                return Some("an invalid address");
            }
            let seg = v.segments()[0];
            if (seg & 0xffc0) == 0xfe80 {
                return Some("a link-local address");
            }
            if !allow_private && (seg & 0xfe00) == 0xfc00 {
                return Some("a private network address");
            }
            None
        }
    }
}

/// Check a source address and resolve its host (no loopback, no metadata endpoints…).
pub async fn validate_source(input: &str, relay: bool) -> AppResult<Source> {
    let input = input.trim();
    let url = reqwest::Url::parse(input).map_err(|_| AppError::BadRequest("enter a full stream address, e.g. rtsp://192.168.1.20:554/stream1".into()))?;
    let scheme = url.scheme().to_ascii_lowercase();
    if !["rtsp", "rtsps", "rtmp", "rtmps", "srt", "udp", "rtp", "http", "https"].contains(&scheme.as_str()) {
        return Err(AppError::BadRequest("use an rtsp://, rtmp://, srt://, udp://, rtp:// or http(s):// address".into()));
    }
    if !relay && !matches!(scheme.as_str(), "http" | "https") {
        return Err(AppError::BadRequest("screens can only play http(s) streams directly: turn relaying on for cameras and encoders".into()));
    }
    let host = url.host_str().filter(|h| !h.is_empty()).ok_or_else(|| AppError::BadRequest("the address has no host".into()))?;
    let host = host.trim_start_matches('[').trim_end_matches(']').to_owned();
    let port = url.port_or_known_default().unwrap_or(match scheme.as_str() { "rtsp" => 554, "rtmp" => 1935, _ => 1 });
    let allow_private = std::env::var("QIVO_STREAM_ALLOW_PRIVATE").map(|v| v != "0").unwrap_or(true);
    let addrs: Vec<IpAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|_| AppError::BadRequest(format!("{host} can't be found (DNS)")))?
            .map(|a| a.ip())
            .collect(),
    };
    for ip in addrs {
        if let Some(why) = blocked_ip(ip, allow_private) {
            return Err(AppError::BadRequest(format!("{host} points to {why}, which isn't allowed")));
        }
    }
    let mut shown = url.clone();
    if shown.password().is_some() {
        let _ = shown.set_password(Some("****"));
    }
    Ok(Source { url: input.to_owned(), scheme, hint: shown.to_string() })
}

/// ffmpeg input options for a source.
fn input_args(src: &Source) -> Vec<String> {
    let mut a: Vec<String> = vec!["-nostdin".into(), "-v".into(), "error".into(), "-protocol_whitelist".into(), whitelist(&src.scheme).into()];
    match src.scheme.as_str() {
        "rtsp" | "rtsps" => a.extend(["-rtsp_transport", "tcp", "-timeout", "15000000"].map(String::from)),
        "http" | "https" | "rtmp" | "rtmps" => a.extend(["-rw_timeout", "15000000"].map(String::from)),
        _ => {}
    }
    a.extend(["-i".into(), src.url.clone()]);
    a
}

// ------------------------------------------------------------------ records

#[derive(sqlx::FromRow, Clone)]
struct Stream {
    id: String,
    org_id: String,
    name: String,
    source_enc: String,
    source_hint: String,
    relay: bool,
    transcode: String,
    video_codec: Option<String>,
    resolution: Option<String>,
    last_error: Option<String>,
}
const COLS: &str = "id, org_id, name, source_enc, source_hint, relay, transcode, video_codec, resolution, last_error";

fn token(s: &Stream) -> String {
    crate::crypto::mac(&format!("stream|{}|{}", s.org_id, s.id))[..32].to_owned()
}

/// What screens use to play this stream.
fn play_url(s: &Stream) -> String {
    if s.relay {
        format!("/api/live/{}/{}/index.m3u8", s.id, token(s))
    } else {
        crate::crypto::open(&s.source_enc).unwrap_or_default()
    }
}

impl Stream {
    fn public(&self) -> Value {
        let running = RELAYS.lock().unwrap().get(&self.id).map(|r| r.child_alive);
        json!({
            "id": self.id, "name": self.name, "source": self.source_hint, "relay": self.relay, "transcode": self.transcode,
            "video_codec": self.video_codec, "resolution": self.resolution, "last_error": self.last_error,
            "status": match running { Some(true) => "live", Some(false) => "starting", None => "idle" },
            "play_url": if self.relay { play_url(self) } else { String::new() },
        })
    }
}

/// For the screen manifest: `{ kind: "stream", url, name }` per stream id.
pub async fn manifest_entries(state: &AppState, org: &str, ids: &[String]) -> AppResult<Vec<(String, Value)>> {
    let mut out = vec![];
    for id in ids {
        if let Some(s) = sqlx::query_as::<_, Stream>(&format!("SELECT {COLS} FROM streams WHERE id = ? AND org_id = ?")).bind(id).bind(org).fetch_optional(&state.db).await? {
            out.push((s.id.clone(), json!({ "id": s.id, "kind": "stream", "name": s.name, "url": play_url(&s) })));
        }
    }
    Ok(out)
}

async fn get_stream(state: &AppState, id: &str) -> AppResult<Stream> {
    sqlx::query_as::<_, Stream>(&format!("SELECT {COLS} FROM streams WHERE id = ?")).bind(id).fetch_optional(&state.db).await?.ok_or(AppError::NotFound)
}

async fn list(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    let rows = sqlx::query_as::<_, Stream>(&format!("SELECT {COLS} FROM streams WHERE org_id = ? ORDER BY name")).bind(user.org()).fetch_all(&state.db).await?;
    Ok(Json(json!(rows.iter().map(Stream::public).collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct StreamReq {
    name: Option<String>,
    source: Option<String>,
    relay: Option<bool>,
    transcode: Option<String>,
}

async fn create(State(state): State<AppState>, user: AuthUser, Json(r): Json<StreamReq>) -> AppResult<Json<Value>> {
    user.require_admin()?;
    let name = r.name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty()).ok_or_else(|| AppError::BadRequest("enter a name".into()))?;
    let relay = r.relay.unwrap_or(true);
    let src = validate_source(r.source.as_deref().unwrap_or(""), relay).await?;
    let transcode = r.transcode.unwrap_or_else(|| "auto".into());
    if !["auto", "always", "never"].contains(&transcode.as_str()) {
        return Err(AppError::BadRequest("transcode must be auto, always or never".into()));
    }
    let id = new_id();
    sqlx::query("INSERT INTO streams (id, org_id, name, source_enc, source_hint, relay, transcode, created_at) VALUES (?,?,?,?,?,?,?,?)")
        .bind(&id)
        .bind(user.org())
        .bind(&name)
        .bind(crate::crypto::seal(&src.url)?)
        .bind(&src.hint)
        .bind(relay)
        .bind(&transcode)
        .bind(now())
        .execute(&state.db)
        .await?;
    audit(&state, user.org(), Some(&user.0.id), "stream.added", &format!("{name} ({})", src.hint)).await;
    Ok(Json(get_stream(&state, &id).await?.public()))
}

async fn update(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>, Json(r): Json<StreamReq>) -> AppResult<Json<Value>> {
    user.require_admin()?;
    let mut s = get_stream(&state, &id).await?;
    if s.org_id != user.org() {
        return Err(AppError::NotFound);
    }
    if let Some(n) = r.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        s.name = n.to_owned();
    }
    if let Some(rl) = r.relay {
        s.relay = rl;
    }
    let source_changed = r.source.as_deref().is_some_and(|v| !v.trim().is_empty() && v.trim() != s.source_hint);
    if source_changed || r.relay.is_some() {
        let url = if source_changed { r.source.clone().unwrap() } else { crate::crypto::open(&s.source_enc)? };
        let src = validate_source(&url, s.relay).await?;
        s.source_enc = crate::crypto::seal(&src.url)?;
        s.source_hint = src.hint;
        s.video_codec = None;
        s.resolution = None;
    }
    if let Some(t) = r.transcode {
        if !["auto", "always", "never"].contains(&t.as_str()) {
            return Err(AppError::BadRequest("transcode must be auto, always or never".into()));
        }
        s.transcode = t;
    }
    sqlx::query("UPDATE streams SET name=?, source_enc=?, source_hint=?, relay=?, transcode=?, video_codec=?, resolution=?, last_error=NULL WHERE id=?")
        .bind(&s.name)
        .bind(&s.source_enc)
        .bind(&s.source_hint)
        .bind(s.relay)
        .bind(&s.transcode)
        .bind(&s.video_codec)
        .bind(&s.resolution)
        .bind(&id)
        .execute(&state.db)
        .await?;
    stop_relay(&id); // the next viewer starts it with the new settings
    state.content_changed(user.org());
    audit(&state, user.org(), Some(&user.0.id), "stream.updated", &s.name).await;
    Ok(Json(get_stream(&state, &id).await?.public()))
}

async fn remove(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    user.require_admin()?;
    let s = get_stream(&state, &id).await?;
    if s.org_id != user.org() {
        return Err(AppError::NotFound);
    }
    let in_use: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlists WHERE org_id = ? AND items LIKE ?").bind(user.org()).bind(format!("%{id}%")).fetch_one(&state.db).await?;
    if in_use > 0 {
        return Err(AppError::Conflict(format!("this stream is used in {in_use} playlist(s); remove it there first")));
    }
    sqlx::query("DELETE FROM streams WHERE id = ?").bind(&id).execute(&state.db).await?;
    stop_relay(&id);
    audit(&state, user.org(), Some(&user.0.id), "stream.removed", &s.name).await;
    Ok(Json(json!({ "ok": true })))
}

/// Connect to the source once and report what it sends.
async fn test(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    user.require_editor()?;
    let s = get_stream(&state, &id).await?;
    if s.org_id != user.org() {
        return Err(AppError::NotFound);
    }
    let src = validate_source(&crate::crypto::open(&s.source_enc)?, s.relay).await?;
    match probe_source(&src).await {
        Ok((codec, res)) => {
            sqlx::query("UPDATE streams SET video_codec=?, resolution=?, last_error=NULL WHERE id=?").bind(&codec).bind(&res).bind(&id).execute(&state.db).await?;
            Ok(Json(json!({ "ok": true, "video_codec": codec, "resolution": res,
                "note": if codec.as_deref() == Some("h264") || !s.relay { "Played as it is (no conversion)." } else { "Converted to H.264 by the server while screens watch." } })))
        }
        Err(e) => {
            sqlx::query("UPDATE streams SET last_error=? WHERE id=?").bind(&e).bind(&id).execute(&state.db).await?;
            Ok(Json(json!({ "ok": false, "error": e })))
        }
    }
}

async fn probe_source(src: &Source) -> Result<(Option<String>, Option<String>), String> {
    let mut args = input_args(src);
    // ffprobe takes the same input options; drop the ffmpeg-only ones.
    args.retain(|a| a != "-nostdin");
    let fut = Command::new(crate::mediaproc::ffprobe_path())
        .args(&args)
        .args(["-select_streams", "v:0", "-show_entries", "stream=codec_name,width,height", "-of", "json"])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(20), fut).await.map_err(|_| "no answer from the source within 20 seconds".to_string())?.map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.lines().last().unwrap_or("the source can't be read").chars().take(300).collect());
    }
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let st = &v["streams"][0];
    let codec = st["codec_name"].as_str().map(str::to_owned);
    let res = st["width"].as_i64().zip(st["height"].as_i64()).map(|(w, h)| format!("{w}×{h}"));
    if codec.is_none() {
        return Err("the source sends no video".into());
    }
    Ok((codec, res))
}

// ------------------------------------------------------------------ relays

struct Relay {
    last_access: Instant,
    child_alive: bool,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    dir: PathBuf,
}

static RELAYS: LazyLock<Mutex<HashMap<String, Relay>>> = LazyLock::new(Default::default);

fn stop_relay(id: &str) {
    if let Some(mut r) = RELAYS.lock().unwrap().remove(id) {
        if let Some(tx) = r.stop.take() {
            let _ = tx.send(());
        }
        let _ = std::fs::remove_dir_all(&r.dir);
    }
}

/// Mark the stream as watched; start its relay if needed.
async fn touch_or_start(state: &AppState, s: &Stream) -> AppResult<PathBuf> {
    let dir = state.data_dir.join("live").join(&s.id);
    {
        let mut map = RELAYS.lock().unwrap();
        if let Some(r) = map.get_mut(&s.id) {
            r.last_access = Instant::now();
            return Ok(r.dir.clone());
        }
        if map.len() >= max_relays() {
            return Err(AppError::TooMany(format!("this server relays at most {} live streams at a time (QIVO_MAX_RELAYS)", max_relays())));
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        map.insert(s.id.clone(), Relay { last_access: Instant::now(), child_alive: false, stop: Some(tx), dir: dir.clone() });
        tokio::spawn(supervise(state.clone(), s.clone(), dir.clone(), rx));
    }
    start_reaper();
    Ok(dir)
}

fn start_reaper() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        tokio::spawn(async {
            let mut tick = tokio::time::interval(Duration::from_secs(5));
            loop {
                tick.tick().await;
                let idle: Vec<String> = RELAYS.lock().unwrap().iter().filter(|(_, r)| r.last_access.elapsed() > IDLE_STOP).map(|(k, _)| k.clone()).collect();
                for id in idle {
                    tracing::info!("live relay {id}: no viewers, stopping");
                    stop_relay(&id);
                }
            }
        });
    });
}

/// Run ffmpeg for one stream; restart it (with back-off) while screens are watching.
async fn supervise(state: AppState, s: Stream, dir: PathBuf, mut stop: tokio::sync::oneshot::Receiver<()>) {
    let mut backoff = Duration::from_secs(2);
    loop {
        if !RELAYS.lock().unwrap().contains_key(&s.id) {
            return;
        }
        let _ = tokio::fs::remove_dir_all(&dir).await;
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            tracing::warn!("live relay {}: {e}", s.name);
            return;
        }
        let started = Instant::now();
        let result = run_relay(&state, &s, &dir, &mut stop).await;
        if let Some(r) = RELAYS.lock().unwrap().get_mut(&s.id) {
            r.child_alive = false;
        }
        match result {
            RelayEnd::Stopped => return,
            RelayEnd::Failed(err) => {
                tracing::warn!("live relay {}: {err}", s.name);
                let _ = sqlx::query("UPDATE streams SET last_error = ? WHERE id = ?").bind(&err).bind(&s.id).execute(&state.db).await;
            }
        }
        backoff = if started.elapsed() > Duration::from_secs(60) { Duration::from_secs(2) } else { (backoff * 2).min(Duration::from_secs(10)) };
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = &mut stop => return,
        }
    }
}

enum RelayEnd {
    Stopped,
    Failed(String),
}

async fn run_relay(state: &AppState, s: &Stream, dir: &std::path::Path, stop: &mut tokio::sync::oneshot::Receiver<()>) -> RelayEnd {
    let url = match crate::crypto::open(&s.source_enc) {
        Ok(u) => u,
        Err(e) => return RelayEnd::Failed(format!("cannot read the source address: {e}")),
    };
    let src = match validate_source(&url, true).await {
        Ok(src) => src,
        Err(e) => return RelayEnd::Failed(e.to_string()),
    };
    // Copy H.264 as it is; convert anything else (H.265 cameras, MPEG-2 IPTV).
    let mut codec = s.video_codec.clone();
    if s.transcode == "auto" && codec.is_none() {
        if let Ok((c, res)) = probe_source(&src).await {
            let _ = sqlx::query("UPDATE streams SET video_codec=?, resolution=? WHERE id=?").bind(&c).bind(&res).bind(&s.id).execute(&state.db).await;
            codec = c;
        }
    }
    let copy = match s.transcode.as_str() {
        "never" => true,
        "always" => false,
        _ => codec.as_deref() == Some("h264"),
    };
    let mut args = input_args(&src);
    args.extend(["-map", "0:v:0", "-map", "0:a:0?", "-sn", "-dn"].map(String::from));
    if copy {
        args.extend(["-c:v", "copy"].map(String::from));
    } else {
        args.extend(
            ["-vf", "scale=w='min(1920,iw)':h='min(1080,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2,format=yuv420p",
             "-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency", "-g", "50", "-keyint_min", "50", "-sc_threshold", "0"]
                .map(String::from),
        );
    }
    args.extend(["-c:a", "aac", "-b:a", "128k", "-ac", "2", "-ar", "48000"].map(String::from));
    // Segment numbers come from the clock, so they keep increasing across restarts and
    // players never see the stream "go backwards" after the camera reconnects.
    let start_number = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 2).unwrap_or(0);
    args.extend(["-f", "hls", "-hls_time", "2", "-hls_list_size", "6", "-hls_flags", "delete_segments+independent_segments+omit_endlist+temp_file"].map(String::from));
    args.extend(["-start_number".into(), start_number.to_string(), "-hls_segment_filename".into()]);
    args.push(dir.join("s%d.ts").to_string_lossy().into_owned());
    args.push(dir.join("index.m3u8").to_string_lossy().into_owned());

    let mut child = match Command::new(crate::mediaproc::ffmpeg_path()).args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true).spawn() {
        Ok(c) => c,
        Err(e) => return RelayEnd::Failed(format!("ffmpeg is not available on the server ({e})")),
    };
    if let Some(r) = RELAYS.lock().unwrap().get_mut(&s.id) {
        r.child_alive = true;
    }
    tracing::info!("live relay {}: started ({})", s.name, if copy { "copy" } else { "H.264" });
    let stderr = child.stderr.take().expect("piped");
    let err_task = tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(stderr).lines();
        let mut last = String::new();
        while let Ok(Some(l)) = lines.next_line().await {
            last = l;
        }
        last
    });
    tokio::select! {
        status = child.wait() => {
            let err = err_task.await.unwrap_or_default();
            let code = status.ok().and_then(|s| s.code());
            RelayEnd::Failed(if err.is_empty() { format!("the source stopped (ffmpeg exit {code:?})") } else { err.chars().take(300).collect() })
        }
        _ = stop => {
            let _ = child.kill().await;
            RelayEnd::Stopped
        }
    }
}

/// Playlist and segments for screens (and the dashboard preview).
async fn serve(State(state): State<AppState>, Path((id, tok, file)): Path<(String, String, String)>) -> AppResult<Response> {
    let is_playlist = file == "index.m3u8";
    let is_segment = file.starts_with('s') && file.ends_with(".ts") && (5..=16).contains(&file.len()) && file[1..file.len() - 3].bytes().all(|b| b.is_ascii_digit());
    if !is_playlist && !is_segment {
        return Err(AppError::NotFound);
    }
    let s = get_stream(&state, &id).await?;
    // Constant-time comparison of the capability token.
    let expected = token(&s);
    if !s.relay || !ct_eq(tok.as_bytes(), expected.as_bytes()) {
        return Err(AppError::NotFound);
    }
    let dir = touch_or_start(&state, &s).await?;
    let path = dir.join(&file);
    if is_playlist {
        let t0 = Instant::now();
        while !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            if t0.elapsed() > START_WAIT {
                let err = sqlx::query_scalar::<_, Option<String>>("SELECT last_error FROM streams WHERE id = ?").bind(&id).fetch_one(&state.db).await.ok().flatten();
                return Ok((StatusCode::SERVICE_UNAVAILABLE, [(header::RETRY_AFTER, "3")], err.unwrap_or_else(|| "the stream is starting".into())).into_response());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    let bytes = tokio::fs::read(&path).await.map_err(|_| AppError::NotFound)?;
    let (ctype, cache) = if is_playlist { ("application/vnd.apple.mpegurl", "no-cache") } else { ("video/mp2t", "max-age=60") };
    Ok(([(header::CONTENT_TYPE, ctype), (header::CACHE_CONTROL, cache)], bytes).into_response())
}

/// Constant-time comparison (no timing hint about how much of a guessed token was right).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sources_are_checked() {
        assert!(validate_source("rtsp://admin:pw@192.168.1.20:554/stream1", true).await.is_ok());
        let s = validate_source("rtsp://admin:secret@192.168.1.20:554/stream1", true).await.unwrap();
        assert!(!s.hint.contains("secret") && s.hint.contains("admin"), "{}", s.hint);
        assert!(validate_source("udp://@239.10.10.1:1234", true).await.is_ok(), "multicast IPTV");
        assert!(validate_source("file:///data/secret.key", true).await.is_err());
        assert!(validate_source("concat:a|b", true).await.is_err());
        assert!(validate_source("http://169.254.169.254/latest/meta-data/", true).await.is_err(), "cloud metadata");
        assert!(validate_source("rtsp://192.168.1.20/x", false).await.is_err(), "cameras must be relayed");
        assert!(validate_source("rtsp://10.0.0.5", true).await.is_ok());
        assert!(validate_source("not a url", true).await.is_err());
    }

    #[test]
    fn private_networks_can_be_refused_for_hosted_servers() {
        assert_eq!(blocked_ip("10.1.2.3".parse().unwrap(), true), None);
        assert!(blocked_ip("10.1.2.3".parse().unwrap(), false).is_some());
        assert!(blocked_ip("100.64.0.1".parse().unwrap(), false).is_some(), "carrier-grade NAT");
        assert!(blocked_ip("169.254.169.254".parse().unwrap(), true).is_some());
        assert!(blocked_ip("::ffff:169.254.169.254".parse().unwrap(), true).is_some(), "IPv4-mapped IPv6");
        assert!(blocked_ip("fe80::1".parse().unwrap(), true).is_some());
        assert_eq!(blocked_ip("239.10.10.1".parse().unwrap(), false), None, "multicast");
    }
}
