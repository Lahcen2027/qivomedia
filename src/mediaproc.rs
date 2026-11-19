//! Media conversion: makes every upload playable on every screen.
//!
//! * Video in formats screens can't decode (AVI, MPEG-2, VOB, MPEG-TS, WMV, FLV, MKV, HEVC…)
//!   → H.264/AAC MP4 (deinterlaced when the source is interlaced, as DVDs and MPEG-2 are).
//! * Audio other than MP3/AAC/Ogg/WAV/FLAC → AAC.
//! * PDF → one JPEG per page (long side 3840 px), played as a slideshow.
//! * TIFF → PNG.
//!
//! Conversions run in the background, a few at a time (`QIVO_TRANSCODE_JOBS`, default 1),
//! report progress live to the dashboard, and resume after a restart. Screens only get a
//! media item once it is ready.
//!
//! Untrusted files go through ffmpeg, so each run is restricted:
//! * only the `file` protocol may be opened (no network, no playlists pointing elsewhere);
//! * the demuxer is forced to the format the upload was identified as, so a file can't
//!   claim to be a playlist or concat list that pulls in other files from the server;
//! * subtitle and data streams are ignored, and a job that stops making progress is killed.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::LazyLock,
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncBufReadExt, process::Command, sync::Semaphore};

use crate::{hub::Event, state::AppState};

fn tool(env: &str, default: &str) -> String {
    std::env::var(env).unwrap_or_else(|_| default.to_owned())
}
pub fn ffmpeg_path() -> String {
    tool("QIVO_FFMPEG", "ffmpeg")
}
pub fn ffprobe_path() -> String {
    tool("QIVO_FFPROBE", "ffprobe")
}
fn ffmpeg() -> String {
    ffmpeg_path()
}
fn ffprobe() -> String {
    ffprobe_path()
}
fn pdftoppm() -> String {
    tool("QIVO_PDFTOPPM", "pdftoppm")
}

static JOBS: LazyLock<Semaphore> = LazyLock::new(|| {
    Semaphore::new(std::env::var("QIVO_TRANSCODE_JOBS").ok().and_then(|v| v.parse().ok()).filter(|n: &usize| *n > 0).unwrap_or(1))
});

/// Longest video side screens receive (4K).
const MAX_SIDE: i64 = 3840;
const MAX_PDF_PAGES: usize = 200;
/// A job that reports no progress for this long is stuck and gets killed.
const STALL: Duration = Duration::from_secs(600);

/// The ffmpeg demuxer for each media type we accept (forced, never auto-detected).
pub fn demuxer_for(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "video/mp4" | "video/quicktime" | "video/3gpp" | "video/x-m4v" | "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "mov",
        "video/webm" | "video/x-matroska" | "audio/webm" => "matroska",
        "video/x-msvideo" | "video/avi" => "avi",
        "video/mpeg" => "mpeg",
        "video/mp2t" => "mpegts",
        "video/x-flv" => "flv",
        "video/x-ms-wmv" | "video/x-ms-asf" | "audio/x-ms-wma" => "asf",
        "video/ogg" | "audio/ogg" => "ogg",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/x-wav" | "audio/wav" | "audio/wave" => "wav",
        "audio/x-flac" | "audio/flac" => "flac",
        "audio/aac" | "audio/x-aac" => "aac",
        "audio/amr" => "amr",
        "image/tiff" => "image2",
        _ => return None,
    })
}

#[derive(Debug, Default, Clone)]
pub struct Probe {
    #[allow(dead_code)] // kept for diagnostics
    pub format: String,
    pub duration_ms: Option<i64>,
    pub vcodec: Option<String>,
    pub pix_fmt: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub interlaced: bool,
    pub acodec: Option<String>,
}

pub async fn probe(path: &Path, demuxer: &str) -> anyhow::Result<Probe> {
    let out = Command::new(ffprobe())
        .args(["-v", "error", "-protocol_whitelist", "file", "-f", demuxer, "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("ffprobe is not available on the server ({e})"))?;
    anyhow::ensure!(out.status.success(), "the file could not be read as {demuxer}: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or(""));
    let v: Value = serde_json::from_slice(&out.stdout)?;
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let video = streams.iter().find(|s| s["codec_type"] == "video" && s["disposition"]["attached_pic"] != 1);
    let audio = streams.iter().find(|s| s["codec_type"] == "audio");
    let format = v["format"]["format_name"].as_str().unwrap_or("");
    // Belt and braces: playlist and concat formats can reference other files.
    anyhow::ensure!(!["hls", "concat", "applehttp", "m3u"].iter().any(|f| format.contains(f)), "this file references other files and can't be used");
    let dur = v["format"]["duration"].as_str().and_then(|d| d.parse::<f64>().ok()).filter(|d| d.is_finite() && *d > 0.0);
    Ok(Probe {
        format: format.to_owned(),
        duration_ms: dur.map(|d| (d * 1000.0).round() as i64),
        vcodec: video.and_then(|s| s["codec_name"].as_str()).map(str::to_owned),
        pix_fmt: video.and_then(|s| s["pix_fmt"].as_str()).map(str::to_owned),
        width: video.and_then(|s| s["width"].as_i64()),
        height: video.and_then(|s| s["height"].as_i64()),
        interlaced: video.and_then(|s| s["field_order"].as_str()).is_some_and(|f| matches!(f, "tt" | "bb" | "tb" | "bt")),
        acodec: audio.and_then(|s| s["codec_name"].as_str()).map(str::to_owned),
    })
}

/// What to do with an upload.
#[derive(Debug, PartialEq)]
pub enum Plan {
    /// Plays as it is.
    Ready,
    /// Needs converting; the media's kind once done.
    Convert(&'static str),
}

fn audio_ok(c: Option<&str>) -> bool {
    matches!(c, None | Some("aac" | "mp3" | "opus" | "vorbis"))
}

/// Can every screen play this file as it is?
pub fn plan_for(mime: &str, p: &Probe) -> Plan {
    let kind = mime.split('/').next().unwrap_or("");
    match kind {
        "video" => {
            let container_ok = matches!(mime, "video/mp4" | "video/quicktime" | "video/x-m4v" | "video/webm");
            let codec_ok = match mime {
                "video/webm" => matches!(p.vcodec.as_deref(), Some("vp8" | "vp9" | "av1")),
                _ => matches!(p.vcodec.as_deref(), Some("h264" | "av1")),
            };
            let pixels_ok = matches!(p.pix_fmt.as_deref(), Some("yuv420p" | "yuvj420p")) && p.width.unwrap_or(0).max(p.height.unwrap_or(0)) <= MAX_SIDE;
            if p.vcodec.is_none() {
                // An audio-only file in a video container: make it audio.
                return Plan::Convert("audio");
            }
            if container_ok && codec_ok && pixels_ok && !p.interlaced && audio_ok(p.acodec.as_deref()) {
                Plan::Ready
            } else {
                Plan::Convert("video")
            }
        }
        "audio" => {
            let native = matches!(
                (mime, p.acodec.as_deref()),
                ("audio/mpeg" | "audio/mp3", Some("mp3")) | ("audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/aac" | "audio/x-aac", Some("aac"))
                    | ("audio/ogg", Some("vorbis" | "opus")) | ("audio/x-wav" | "audio/wav" | "audio/wave", Some(_)) | ("audio/x-flac" | "audio/flac", Some("flac"))
            );
            if native { Plan::Ready } else { Plan::Convert("audio") }
        }
        _ => Plan::Ready,
    }
}

/// Queue a media item for conversion (runs in the background).
pub fn enqueue(state: &AppState, id: &str) {
    let (state, id) = (state.clone(), id.to_owned());
    tokio::spawn(async move {
        let _permit = JOBS.acquire().await;
        process(&state, &id).await;
    });
}

/// After a restart: pick up conversions that were interrupted.
pub async fn resume(state: &AppState) {
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM media WHERE status = 'processing'").fetch_all(&state.db).await.unwrap_or_default();
    if !ids.is_empty() {
        tracing::info!("resuming {} media conversion(s)", ids.len());
    }
    for id in ids {
        enqueue(state, &id);
    }
}

#[derive(sqlx::FromRow)]
struct Job {
    org_id: String,
    kind: String,
    mime: String,
    name: String,
    source_file: Option<String>,
    duration_ms: Option<i64>,
}

async fn process(state: &AppState, id: &str) {
    let job: Option<Job> = sqlx::query_as("SELECT org_id, kind, mime, name, source_file, duration_ms FROM media WHERE id = ? AND status = 'processing'")
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some(job) = job else { return };
    let Some(src_name) = job.source_file.clone() else {
        fail(state, &job.org_id, id, "the original file is missing").await;
        return;
    };
    let src = state.data_dir.join("media-src").join(&src_name);
    let started = Instant::now();
    let result = match job.kind.as_str() {
        "pdf" => convert_pdf(state, &job, id, &src).await,
        _ => convert_av(state, &job, id, &src).await,
    };
    let _ = tokio::fs::remove_file(&src).await;
    match result {
        Ok(()) => tracing::info!("converted {} ({}) in {:.1}s", job.name, job.kind, started.elapsed().as_secs_f64()),
        Err(e) => {
            tracing::warn!("conversion of {} failed: {e}", job.name);
            fail(state, &job.org_id, id, &e.to_string()).await;
        }
    }
}

async fn fail(state: &AppState, org: &str, id: &str, msg: &str) {
    let msg: String = msg.chars().take(500).collect();
    let _ = sqlx::query("UPDATE media SET status = 'failed', error = ?, source_file = NULL WHERE id = ?").bind(&msg).bind(id).execute(&state.db).await;
    state.publish(Event::to_admins(org, json!({ "type": "media_progress", "id": id, "status": "failed", "error": msg })));
}

async fn report(state: &AppState, org: &str, id: &str, pct: i64) {
    let _ = sqlx::query("UPDATE media SET progress = ? WHERE id = ? AND status = 'processing'").bind(pct).bind(id).execute(&state.db).await;
    state.publish(Event::to_admins(org, json!({ "type": "media_progress", "id": id, "status": "processing", "progress": pct })));
}

/// Move a finished output into the content-addressed store; returns (file name, sha256, size).
async fn store(state: &AppState, path: &Path, ext: &str) -> anyhow::Result<(String, String, i64)> {
    let bytes = tokio::fs::read(path).await?;
    let sha = hex::encode(Sha256::digest(&bytes));
    let name = format!("{sha}.{ext}");
    let dest = state.data_dir.join("media").join(&name);
    if tokio::fs::try_exists(&dest).await.unwrap_or(false) {
        tokio::fs::remove_file(path).await?;
    } else {
        tokio::fs::rename(path, &dest).await?;
    }
    Ok((name, sha, bytes.len() as i64))
}

/// The media was deleted while converting: drop the result.
async fn still_wanted(state: &AppState, id: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media WHERE id = ?").bind(id).fetch_one(&state.db).await.unwrap_or(0) > 0
}

async fn finish(state: &AppState, org: &str, id: &str) {
    state.publish(Event::to_admins(org, json!({ "type": "media_progress", "id": id, "status": "ready", "progress": 100 })));
    // Screens showing a playlist that already includes this item now get it.
    state.content_changed(org);
}

fn work_dir(state: &AppState, id: &str) -> PathBuf {
    state.data_dir.join("tmp").join(format!("job-{id}"))
}

async fn convert_av(state: &AppState, job: &Job, id: &str, src: &Path) -> anyhow::Result<()> {
    let demuxer = demuxer_for(&job.mime).ok_or_else(|| anyhow::anyhow!("unsupported format {}", job.mime))?;
    let p = probe(src, demuxer).await?;
    let dir = work_dir(state, id);
    tokio::fs::create_dir_all(&dir).await?;
    let (ext, out_kind, mime) = match job.kind.as_str() {
        "audio" => ("m4a", "audio", "audio/mp4"),
        "image" => ("png", "image", "image/png"),
        _ => ("mp4", "video", "video/mp4"),
    };
    let out = dir.join(format!("out.{ext}"));
    let mut args: Vec<String> = vec!["-nostdin".into(), "-y".into(), "-v".into(), "error".into(), "-protocol_whitelist".into(), "file,pipe".into(), "-f".into(), demuxer.into()];
    args.extend(["-i".into(), src.to_string_lossy().into_owned()]);
    match out_kind {
        "image" => args.extend(["-frames:v", "1"].map(String::from)),
        "audio" => args.extend(["-vn", "-sn", "-dn", "-map", "0:a:0", "-c:a", "aac", "-b:a", "192k"].map(String::from)),
        _ => {
            args.extend(["-sn", "-dn", "-map", "0:v:0", "-map", "0:a:0?"].map(String::from));
            let too_big = p.width.unwrap_or(0).max(p.height.unwrap_or(0)) > MAX_SIDE;
            let copy_video = p.vcodec.as_deref() == Some("h264") && matches!(p.pix_fmt.as_deref(), Some("yuv420p" | "yuvj420p")) && !p.interlaced && !too_big;
            if copy_video {
                args.extend(["-c:v", "copy"].map(String::from));
            } else {
                let mut filters = vec![];
                if p.interlaced {
                    filters.push("yadif=mode=send_frame:deint=interlaced".to_owned());
                }
                // Fit within 3840×3840, keep the aspect ratio, even dimensions for H.264.
                filters.push(format!("scale=w='min({MAX_SIDE},iw)':h='min({MAX_SIDE},ih)':force_original_aspect_ratio=decrease:force_divisible_by=2"));
                filters.push("format=yuv420p".into());
                args.extend(["-vf".into(), filters.join(","), "-c:v".into(), "libx264".into(), "-preset".into(), "veryfast".into(), "-crf".into(), "20".into(), "-profile:v".into(), "high".into()]);
            }
            if matches!(p.acodec.as_deref(), Some("aac")) {
                args.extend(["-c:a", "copy"].map(String::from));
            } else {
                args.extend(["-c:a", "aac", "-b:a", "192k", "-ac", "2"].map(String::from));
            }
            args.extend(["-movflags", "+faststart"].map(String::from));
        }
    }
    args.extend(["-progress".into(), "pipe:1".into(), "-nostats".into(), out.to_string_lossy().into_owned()]);
    run_ffmpeg(state, &job.org_id, id, &args, p.duration_ms).await.inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&dir);
    })?;
    if !still_wanted(state, id).await {
        let _ = tokio::fs::remove_dir_all(&dir).await;
        return Ok(());
    }
    // What the screens will get (dimensions/duration of the output).
    let out_demux = match ext { "m4a" | "mp4" => "mov", _ => "image2" };
    let op = probe(&out, out_demux).await.unwrap_or_default();
    let (file, sha, size) = store(state, &out, ext).await?;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    // A still image has no duration (its playlist item decides how long it shows).
    let duration = if out_kind == "image" { None } else { job.duration_ms.or(op.duration_ms).or(p.duration_ms) };
    sqlx::query("UPDATE media SET status='ready', progress=100, error=NULL, source_file=NULL, kind=?, mime=?, file_name=?, sha256=?, size=?, duration_ms=?, width=?, height=? WHERE id=?")
        .bind(out_kind)
        .bind(mime)
        .bind(&file)
        .bind(&sha)
        .bind(size)
        .bind(duration)
        .bind(op.width.or(p.width))
        .bind(op.height.or(p.height))
        .bind(id)
        .execute(&state.db)
        .await?;
    finish(state, &job.org_id, id).await;
    Ok(())
}

async fn run_ffmpeg(state: &AppState, org: &str, id: &str, args: &[String], duration_ms: Option<i64>) -> anyhow::Result<()> {
    let mut child = Command::new(ffmpeg())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("ffmpeg is not available on the server ({e})"))?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let err_task = tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(stderr).lines();
        let mut last = Vec::new();
        while let Ok(Some(l)) = lines.next_line().await {
            last.push(l);
            if last.len() > 5 {
                last.remove(0);
            }
        }
        last.join(" | ")
    });
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let (mut last_pct, mut last_report) = (-1i64, Instant::now());
    loop {
        match tokio::time::timeout(STALL, lines.next_line()).await {
            Err(_) => {
                let _ = child.kill().await;
                anyhow::bail!("the conversion stopped making progress and was cancelled");
            }
            Ok(Ok(Some(line))) => {
                if let (Some(us), Some(total)) = (line.strip_prefix("out_time_us=").and_then(|v| v.parse::<i64>().ok()), duration_ms) {
                    let pct = (us / 10 / total.max(1)).clamp(0, 99);
                    if pct != last_pct && last_report.elapsed() >= Duration::from_millis(800) {
                        report(state, org, id, pct).await;
                        (last_pct, last_report) = (pct, Instant::now());
                    }
                }
            }
            Ok(_) => break,
        }
    }
    let status = child.wait().await?;
    let err = err_task.await.unwrap_or_default();
    anyhow::ensure!(status.success(), "conversion failed: {}", if err.is_empty() { "unknown error".into() } else { err });
    Ok(())
}

async fn convert_pdf(state: &AppState, job: &Job, id: &str, src: &Path) -> anyhow::Result<()> {
    let dir = work_dir(state, id);
    tokio::fs::create_dir_all(&dir).await?;
    let pages = pdf_page_count(src).await?;
    anyhow::ensure!(pages > 0, "this PDF has no pages");
    let render = pages.min(MAX_PDF_PAGES);
    let mut files = Vec::with_capacity(render);
    let (mut w, mut h) = (None, None);
    for page in 1..=render {
        let prefix = dir.join(format!("p{page}"));
        let out = Command::new(pdftoppm())
            .args(["-q", "-jpeg", "-jpegopt", "quality=90", "-scale-to", &MAX_SIDE.to_string(), "-singlefile", "-f", &page.to_string(), "-l", &page.to_string()])
            .arg(src)
            .arg(&prefix)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("the PDF renderer (pdftoppm) is not available on the server ({e})"))?;
        anyhow::ensure!(out.status.success(), "page {page} could not be rendered: {}", String::from_utf8_lossy(&out.stderr).trim());
        let jpg = dir.join(format!("p{page}.jpg"));
        if page == 1 {
            if let Ok(img) = probe(&jpg, "image2").await {
                (w, h) = (img.width, img.height);
            }
        }
        let (file, _, _) = store(state, &jpg, "jpg").await?;
        files.push(file);
        report(state, &job.org_id, id, (page * 100 / render).min(99) as i64).await;
    }
    let _ = tokio::fs::remove_dir_all(&dir).await;
    if !still_wanted(state, id).await {
        return Ok(());
    }
    let note = (pages > MAX_PDF_PAGES).then(|| format!("Only the first {MAX_PDF_PAGES} of {pages} pages are shown."));
    let src_bytes = tokio::fs::metadata(src).await.map(|m| m.len() as i64).unwrap_or(0);
    sqlx::query("UPDATE media SET status='ready', progress=100, error=?, source_file=NULL, file_name=?, pages=?, width=?, height=?, size=? WHERE id=?")
        .bind(note)
        .bind(&files[0])
        .bind(serde_json::to_string(&files)?)
        .bind(w)
        .bind(h)
        .bind(src_bytes)
        .bind(id)
        .execute(&state.db)
        .await?;
    finish(state, &job.org_id, id).await;
    Ok(())
}

async fn pdf_page_count(src: &Path) -> anyhow::Result<usize> {
    // pdfinfo ships with pdftoppm (poppler-utils).
    let pdfinfo = pdftoppm().replace("pdftoppm", "pdfinfo");
    let out = Command::new(&pdfinfo).arg(src).stdin(Stdio::null()).output().await.map_err(|e| anyhow::anyhow!("pdfinfo is not available on the server ({e})"))?;
    anyhow::ensure!(out.status.success(), "this file could not be read as a PDF (it may be damaged or password-protected)");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("Pages:").map(|v| v.trim().parse().unwrap_or(0)))
        .ok_or_else(|| anyhow::anyhow!("could not count the pages of this PDF"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(v: &str, pix: &str, a: Option<&str>) -> Probe {
        Probe { vcodec: Some(v.into()), pix_fmt: Some(pix.into()), width: Some(1920), height: Some(1080), acodec: a.map(Into::into), ..Default::default() }
    }
    #[test]
    fn decides_what_needs_converting() {
        assert_eq!(plan_for("video/mp4", &p("h264", "yuv420p", Some("aac"))), Plan::Ready);
        assert_eq!(plan_for("video/webm", &p("vp9", "yuv420p", Some("opus"))), Plan::Ready);
        assert_eq!(plan_for("video/x-msvideo", &p("mpeg4", "yuv420p", Some("mp3"))), Plan::Convert("video"), "AVI");
        assert_eq!(plan_for("video/mpeg", &p("mpeg2video", "yuv420p", Some("mp2"))), Plan::Convert("video"), "MPEG-2 / VOB");
        assert_eq!(plan_for("video/mp4", &p("hevc", "yuv420p", Some("aac"))), Plan::Convert("video"), "HEVC");
        assert_eq!(plan_for("video/mp4", &p("h264", "yuv420p10le", Some("aac"))), Plan::Convert("video"), "10-bit H.264");
        assert_eq!(plan_for("video/mp4", &p("h264", "yuv420p", Some("ac3"))), Plan::Convert("video"), "AC-3 audio");
        let mut interlaced = p("h264", "yuv420p", Some("aac"));
        interlaced.interlaced = true;
        assert_eq!(plan_for("video/mp4", &interlaced), Plan::Convert("video"));
        let mut eight_k = p("h264", "yuv420p", None);
        eight_k.width = Some(7680);
        assert_eq!(plan_for("video/mp4", &eight_k), Plan::Convert("video"), "larger than 4K");
        let audio = Probe { acodec: Some("mp3".into()), ..Default::default() };
        assert_eq!(plan_for("audio/mpeg", &audio), Plan::Ready);
        let wma = Probe { acodec: Some("wmav2".into()), ..Default::default() };
        assert_eq!(plan_for("audio/x-ms-wma", &wma), Plan::Convert("audio"));
        assert_eq!(plan_for("image/jpeg", &Probe::default()), Plan::Ready);
    }
}
