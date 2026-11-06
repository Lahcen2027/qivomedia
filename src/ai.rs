//! AI layout agent: turns a plain-language brief into a real, editable Qivomedia
//! layout (zones + playlists + widgets) using Claude with a forced tool call.
//!
//! Everything it makes is a *draft*: a new layout that no schedule or screen uses
//! until a person previews it and assigns it.

use std::time::Duration;

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    auth::{audit, AuthUser},
    content::{fetch_layout, normalize_items, normalize_regions},
    error::{AppError, AppResult},
    media::{Media, MEDIA_COLS},
    platform::month_start,
    state::AppState,
    util::{new_id, now},
};

const DEFAULT_MODEL: &str = "claude-sonnet-5-5";
const DEFAULT_BASE: &str = "https://api.anthropic.com";
const MAX_PROMPT: usize = 4000;
const MAX_HTML: usize = 60_000;

fn api_key() -> Option<String> {
    std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.trim().is_empty())
}
pub fn configured() -> bool {
    api_key().is_some()
}
fn model() -> String {
    std::env::var("QIVO_AI_MODEL").ok().filter(|m| !m.is_empty()).unwrap_or_else(|| DEFAULT_MODEL.into())
}
fn base_url() -> String {
    std::env::var("QIVO_AI_BASE_URL").ok().filter(|m| !m.is_empty()).unwrap_or_else(|| DEFAULT_BASE.into())
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ai/status", get(status))
        .route("/ai/layouts", post(generate))
}

async fn quota(state: &AppState, org: &str) -> AppResult<(bool, i64, i64)> {
    let (enabled, limit): (bool, i64) = sqlx::query_as("SELECT ai_enabled, ai_monthly_limit FROM organizations WHERE id = ?")
        .bind(org)
        .fetch_one(&state.db)
        .await?;
    let used: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ai_usage WHERE org_id = ? AND ok = 1 AND created_at >= ?")
        .bind(org)
        .bind(month_start())
        .fetch_one(&state.db)
        .await?;
    Ok((enabled, used, limit))
}

async fn status(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    let (enabled, used, limit) = quota(&state, user.org()).await?;
    Ok(Json(json!({
        "available": configured() && enabled,
        "configured": configured(),
        "enabled_for_org": enabled,
        "used_this_month": used,
        "monthly_limit": limit,
    })))
}

// ---------------------------------------------------------------- prompt + tool

fn tool_schema() -> Value {
    json!({
      "name": "build_signage",
      "description": "Deliver the complete digital-signage screen design: the layout canvas, its zones, and one playlist of items per zone.",
      "input_schema": {
        "type": "object",
        "required": ["layout_name", "background", "regions", "summary"],
        "properties": {
          "layout_name": { "type": "string", "description": "Short descriptive name, in the user's language." },
          "background": { "type": "string", "description": "Canvas background as a hex color, e.g. #0b0e15." },
          "summary": { "type": "string", "description": "1–3 sentences for the user: what you built and anything they must fill in (placeholders). User's language." },
          "regions": {
            "type": "array", "minItems": 1, "maxItems": 8,
            "items": {
              "type": "object",
              "required": ["name", "x", "y", "w", "h", "playlist"],
              "properties": {
                "name": { "type": "string" },
                "x": { "type": "number", "minimum": 0, "maximum": 100, "description": "Left edge, % of canvas width" },
                "y": { "type": "number", "minimum": 0, "maximum": 100, "description": "Top edge, % of canvas height" },
                "w": { "type": "number", "minimum": 1, "maximum": 100 },
                "h": { "type": "number", "minimum": 1, "maximum": 100 },
                "z": { "type": "integer", "description": "Layer; higher draws on top. Only for intentional overlays." },
                "transition": { "type": "string", "enum": ["fade", "slide", "zoom", "none"] },
                "background": { "type": "string" },
                "playlist": {
                  "type": "object",
                  "required": ["name", "items"],
                  "properties": {
                    "name": { "type": "string" },
                    "shuffle": { "type": "boolean" },
                    "items": {
                      "type": "array", "minItems": 1, "maxItems": 20,
                      "items": {
                        "type": "object",
                        "required": ["type", "duration"],
                        "properties": {
                          "type": { "type": "string", "enum": ["html", "text", "ticker", "clock", "weather", "countdown", "media", "webpage"] },
                          "duration": { "type": "number", "minimum": 3, "maximum": 3600, "description": "Seconds on screen" },
                          "html": { "type": "string", "description": "type=html: complete self-contained slide markup (inline <style> allowed)." },
                          "text": { "type": "string" },
                          "size": { "type": "number", "description": "Font size in px at the canvas resolution" },
                          "color": { "type": "string" },
                          "bg": { "type": "string" },
                          "align": { "type": "string", "enum": ["left", "center", "right"] },
                          "bold": { "type": "boolean" },
                          "speed": { "type": "number", "description": "ticker px/second" },
                          "format": { "type": "string", "enum": ["24h", "12h"] },
                          "locale": { "type": "string", "description": "clock locale, e.g. fr-MA, ar-MA, en-GB" },
                          "show_date": { "type": "boolean" },
                          "lat": { "type": "number" }, "lon": { "type": "number" },
                          "label": { "type": "string" },
                          "units": { "type": "string", "enum": ["c", "f"] },
                          "target": { "type": "string", "description": "countdown target, YYYY-MM-DDTHH:MM" },
                          "media_id": { "type": "string", "description": "type=media: an id from the provided media library ONLY" },
                          "fit": { "type": "string", "enum": ["contain", "cover", "fill"] },
                          "url": { "type": "string", "description": "type=webpage: https URL the user gave" },
                          "when": {
                            "type": "object",
                            "description": "Optional day-parting: only play this item at these times",
                            "properties": {
                              "days_mask": { "type": "integer", "description": "bit0=Mon … bit6=Sun; 127 = every day" },
                              "time_from": { "type": "string", "description": "HH:MM" },
                              "time_to": { "type": "string", "description": "HH:MM" }
                            }
                          }
                        }
                      }
                    }
                  }
                }
              }
            }
          }
        }
      }
    })
}

fn system_prompt(width: i64, height: i64, language: &str) -> String {
    format!(
r#"You are the design agent inside Qivomedia, a professional digital-signage platform. You design screens that are shown on TVs and displays in public places (campuses, shops, restaurants, offices), read from a distance, often in a few seconds.

You deliver by calling the build_signage tool exactly once with a complete design.

CANVAS: {width}×{height} px. Zone coordinates are percentages of this canvas. All font sizes are px at this resolution.

HOW QIVOMEDIA WORKS
- A layout is split into zones (regions). Each zone plays its own playlist in a loop, with a transition between items.
- Item types:
  • html — a designed slide you write as self-contained HTML+CSS. It fills its zone (100% width/height). Use this for posters, menus, announcements, promos, schedules.
  • text — one block of large text (size, color, bg, align, bold).
  • ticker — scrolling text line, ideal for a bottom bar.
  • clock — live time/date (format 24h/12h, locale).
  • weather — live weather for lat/lon with a label.
  • countdown — live countdown to a date-time.
  • media — an image or video from the user's library, by media_id (only ids listed below).
  • webpage — a live web page; only if the user gives the URL.
- Text, ticker, html and countdown support {{{{placeholders}}}} that triggers can fill at run time (e.g. {{{{name}}}}).

DESIGN RULES
- Legibility first: at 1920×1080, body text ≥ 42px, headlines 90–180px, nothing under 32px. Scale proportionally for other resolutions (portrait screens are narrower: shorter lines).
- High contrast (WCAG AA or better). Bold, simple hierarchy: one message per slide, max ~25 words.
- Safe margins: keep content ≥ 4% from screen edges.
- Use native widgets for live data (clock, weather, ticker, countdown) instead of faking them in HTML.
- A full-screen promo layout is fine; a typical info layout is main zone + side info + bottom ticker. Zones must not overlap unless you set z for an intentional overlay. Zones should tile the canvas without gaps unless the background is intended to show.
- Durations: 8–15 s for slides, 20–60 s for tickers and clocks.

HTML SLIDE RULES (type=html)
- Self-contained: inline <style> only. No external scripts, images, fonts or URLs (screens may be offline). Use system fonts: font-family: system-ui, "Segoe UI", Roboto, "Noto Sans Arabic", sans-serif.
- Root element must fill the zone: html,body{{margin:0;height:100%;overflow:hidden}} and size with %, vh/vw (vw/vh refer to the ZONE, not the screen) or px computed from the zone's pixel size.
- Visuals: CSS gradients, shapes, inline SVG icons/illustrations. Photos only via media items from the library.
- Arabic text: wrap in dir="rtl" and lang="ar". Keep markup under 20 KB per slide.

CONTENT RULES
- Never invent facts the user did not give: prices, phone numbers, dates, names, statistics. Use clear placeholders in square brackets like [PRICE] or [DATE] and mention them in the summary.
- Write all on-screen copy in: {language}. Write the summary in the same language as the user's brief.
- Weather: if the user names a city, use its approximate coordinates.
"#
    )
}

#[derive(Deserialize)]
struct GenerateReq {
    prompt: String,
    #[serde(default = "d_w")]
    width: i64,
    #[serde(default = "d_h")]
    height: i64,
    #[serde(default = "d_lang")]
    language: String,
    #[serde(default = "yes")]
    use_media: bool,
    /// Revise this existing layout instead of creating a new one.
    layout_id: Option<String>,
}
fn d_w() -> i64 { 1920 }
fn d_h() -> i64 { 1080 }
fn d_lang() -> String { "the same language as the brief".into() }
fn yes() -> bool { true }

async fn record(state: &AppState, org: &str, user: &str, kind: &str, prompt: &str, layout: Option<&str>, tokens: (i64, i64), err: Option<&str>) {
    let _ = sqlx::query(
        "INSERT INTO ai_usage (org_id, user_id, kind, prompt, layout_id, input_tokens, output_tokens, ok, error, created_at)
         VALUES (?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(org).bind(user).bind(kind).bind(prompt.chars().take(500).collect::<String>()).bind(layout)
    .bind(tokens.0).bind(tokens.1).bind(err.is_none()).bind(err).bind(now())
    .execute(&state.db).await;
}

async fn call_claude(system: &str, user_msg: &str) -> Result<(Value, (i64, i64)), String> {
    let key = api_key().ok_or("AI is not configured on this server")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(240))
        .build()
        .map_err(|e| e.to_string())?;
    let body = json!({
        "model": model(),
        "max_tokens": 16000,
        "system": system,
        "tools": [tool_schema()],
        "tool_choice": { "type": "tool", "name": "build_signage" },
        "messages": [{ "role": "user", "content": user_msg }],
    });
    let res = client
        .post(format!("{}/v1/messages", base_url()))
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await
        .map_err(|e| if e.is_timeout() { "the AI took too long — try a simpler brief".to_string() } else { format!("could not reach the AI service: {e}") })?;
    let status = res.status();
    let v: Value = res.json().await.map_err(|e| format!("bad AI response: {e}"))?;
    if !status.is_success() {
        let msg = v["error"]["message"].as_str().unwrap_or("unknown error");
        tracing::warn!("anthropic error {status}: {msg}");
        return Err(match status.as_u16() {
            401 | 403 => "the AI key on this server is invalid".into(),
            429 | 529 => "the AI service is busy — try again in a minute".into(),
            _ => format!("AI service error ({status})"),
        });
    }
    let tokens = (
        v["usage"]["input_tokens"].as_i64().unwrap_or(0),
        v["usage"]["output_tokens"].as_i64().unwrap_or(0),
    );
    if v["stop_reason"] == "max_tokens" {
        return Err("the design was too large — ask for fewer slides".into());
    }
    let input = v["content"]
        .as_array()
        .and_then(|blocks| blocks.iter().find(|b| b["type"] == "tool_use" && b["name"] == "build_signage"))
        .map(|b| b["input"].clone())
        .ok_or("the AI did not return a design")?;
    Ok((input, tokens))
}

/// Strip ids/derived fields so the model sees a clean, compact current design.
fn describe_existing(layout: &Value, playlists: &serde_json::Map<String, Value>) -> Value {
    let regions: Vec<Value> = layout["regions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let pl = r["playlist_id"].as_str().and_then(|p| playlists.get(p));
            let items: Vec<Value> = pl
                .and_then(|p| p["items"].as_array().cloned())
                .unwrap_or_default()
                .into_iter()
                .map(|mut it| {
                    if let Some(o) = it.as_object_mut() { o.remove("id"); }
                    it
                })
                .collect();
            json!({
                "name": r["name"], "x": r["x"], "y": r["y"], "w": r["w"], "h": r["h"], "z": r["z"],
                "transition": r["transition"],
                "playlist": { "name": pl.map(|p| p["name"].clone()).unwrap_or(json!("Playlist")), "items": items },
            })
        })
        .collect();
    json!({ "layout_name": layout["name"], "background": layout["background"], "regions": regions })
}

async fn generate(State(state): State<AppState>, user: AuthUser, Json(req): Json<GenerateReq>) -> AppResult<Json<Value>> {
    user.require_editor()?;
    let org = user.org().to_string();
    if !configured() {
        return Err(AppError::BadRequest("AI is not configured on this server (ANTHROPIC_API_KEY)".into()));
    }
    let (enabled, used, limit) = quota(&state, &org).await?;
    if !enabled {
        return Err(AppError::Forbidden("AI is not included in your plan".into()));
    }
    if used >= limit {
        return Err(AppError::Forbidden(format!("you've used all {limit} AI designs for this month")));
    }
    let prompt = req.prompt.trim();
    if prompt.len() < 5 {
        return Err(AppError::BadRequest("describe the screen you want in a few words".into()));
    }
    if prompt.len() > MAX_PROMPT {
        return Err(AppError::BadRequest("brief is too long (max 4000 characters)".into()));
    }

    // Revising: work from the existing design at its own resolution.
    let existing = match &req.layout_id {
        Some(id) => Some(fetch_layout(&state, &org, id).await?),
        None => None,
    };
    let (width, height) = existing.as_ref().map(|l| (l.width, l.height)).unwrap_or((req.width, req.height));
    if !(320..=8192).contains(&width) || !(320..=8192).contains(&height) {
        return Err(AppError::BadRequest("resolution out of range".into()));
    }

    let media: Vec<Media> = if req.use_media {
        sqlx::query_as(&format!("SELECT {MEDIA_COLS} FROM media WHERE org_id = ? AND kind IN ('image','video') ORDER BY created_at DESC LIMIT 150"))
            .bind(&org)
            .fetch_all(&state.db)
            .await?
    } else {
        vec![]
    };
    let media_list = if media.is_empty() {
        "(no media library items available — design with HTML, text and widgets only)".to_string()
    } else {
        media
            .iter()
            .map(|m| format!("- id={} | {} | {}{}", m.id, m.kind, m.name, m.duration_ms.map(|d| format!(" | {}s", d / 1000)).unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let mut user_msg = format!("MEDIA LIBRARY (use only these ids for media items):\n{media_list}\n\n");
    let kind = if let Some(l) = &existing {
        let lj = l.to_json();
        let mut pls = serde_json::Map::new();
        for r in lj["regions"].as_array().into_iter().flatten() {
            if let Some(pid) = r["playlist_id"].as_str().filter(|s| !s.is_empty()) {
                if let Ok(Some(p)) = sqlx::query_as::<_, crate::content::Playlist>(
                    "SELECT id, name, items, shuffle, updated_at FROM playlists WHERE id = ? AND org_id = ?",
                ).bind(pid).bind(&org).fetch_optional(&state.db).await {
                    pls.insert(pid.to_string(), p.to_json());
                }
            }
        }
        user_msg.push_str(&format!(
            "CURRENT DESIGN (JSON):\n{}\n\nCHANGE REQUEST: {prompt}\n\nReturn the complete updated design. Keep everything the request doesn't ask to change.",
            describe_existing(&lj, &pls)
        ));
        "revise"
    } else {
        user_msg.push_str(&format!("BRIEF: {prompt}"));
        "create"
    };

    let system = system_prompt(width, height, &req.language);
    let (design, tokens) = match call_claude(&system, &user_msg).await {
        Ok(r) => r,
        Err(e) => {
            record(&state, &org, &user.0.id, kind, prompt, req.layout_id.as_deref(), (0, 0), Some(&e)).await;
            return Err(AppError::BadRequest(e));
        }
    };

    match apply_design(&state, &org, &design, width, height, existing.as_ref().map(|l| l.id.as_str())).await {
        Ok(layout_id) => {
            record(&state, &org, &user.0.id, kind, prompt, Some(&layout_id), tokens, None).await;
            audit(&state, &org, Some(&user.0.id), if kind == "create" { "ai.layout_created" } else { "ai.layout_revised" }, prompt).await;
            if kind == "revise" {
                state.content_changed(&org);
            }
            let layout = fetch_layout(&state, &org, &layout_id).await?.to_json();
            Ok(Json(json!({ "layout": layout, "summary": design["summary"], "kind": kind })))
        }
        Err(e) => {
            let msg = format!("the AI design could not be used: {e}");
            record(&state, &org, &user.0.id, kind, prompt, req.layout_id.as_deref(), tokens, Some(&msg)).await;
            Err(AppError::BadRequest(msg))
        }
    }
}

/// Validate the model's design with the same rules as the editor, then store it.
async fn apply_design(state: &AppState, org: &str, d: &Value, width: i64, height: i64, revise: Option<&str>) -> AppResult<String> {
    let name = d["layout_name"].as_str().unwrap_or("AI layout").chars().take(120).collect::<String>();
    let background = d["background"].as_str().filter(|b| b.starts_with('#') && b.len() <= 9).unwrap_or("#000000").to_string();
    let regions_in = d["regions"].as_array().ok_or_else(|| AppError::BadRequest("no zones".into()))?;
    if regions_in.is_empty() {
        return Err(AppError::BadRequest("no zones".into()));
    }

    let mut tx_playlists: Vec<(String, String, String, bool)> = vec![]; // id, name, items, shuffle
    let mut regions = vec![];
    for (i, r) in regions_in.iter().enumerate().take(8) {
        let pl = &r["playlist"];
        let mut items: Vec<Value> = pl["items"].as_array().cloned().unwrap_or_default();
        items.truncate(20);
        for it in items.iter_mut() {
            if let Some(o) = it.as_object_mut() {
                // Drop media references the model invented; keep html bounded.
                if let Some(h) = o.get("html").and_then(Value::as_str) {
                    if h.len() > MAX_HTML {
                        return Err(AppError::BadRequest("a slide is too large".into()));
                    }
                }
                o.remove("id");
            }
        }
        items.retain(|it| it["type"] != "media" || it["media_id"].as_str().is_some());
        let mut valid_items = vec![];
        for it in items {
            // Skip individual items that fail validation (e.g. unknown media id) rather than failing the whole design.
            if let Ok(v) = normalize_items(state, org, std::slice::from_ref(&it)).await {
                valid_items.extend(v.as_array().cloned().unwrap_or_default());
            }
        }
        if valid_items.is_empty() {
            continue;
        }
        let pid = new_id();
        let pname = pl["name"].as_str().unwrap_or("Playlist").chars().take(120).collect::<String>();
        tx_playlists.push((pid.clone(), format!("{name} · {pname}"), Value::Array(valid_items).to_string(), pl["shuffle"].as_bool().unwrap_or(false)));
        let clamp = |v: &Value, lo: f64, hi: f64| v.as_f64().unwrap_or(0.0).clamp(lo, hi);
        let x = clamp(&r["x"], 0.0, 99.0);
        let y = clamp(&r["y"], 0.0, 99.0);
        regions.push(json!({
            "name": r["name"].as_str().unwrap_or("Zone"),
            "x": x, "y": y,
            "w": clamp(&r["w"], 1.0, 100.0 - x), "h": clamp(&r["h"], 1.0, 100.0 - y),
            "z": r["z"].as_i64().unwrap_or(i as i64),
            "transition": r["transition"].as_str().filter(|t| ["fade", "slide", "zoom", "none"].contains(t)).unwrap_or("fade"),
            "background": r["background"].as_str().unwrap_or(""),
            "playlist_id": pid,
        }));
    }
    if regions.is_empty() {
        return Err(AppError::BadRequest("the design had no usable content".into()));
    }

    // Store playlists first (regions reference them), then the layout — one transaction.
    let mut tx = state.db.begin().await?;
    for (id, pname, items, shuffle) in &tx_playlists {
        sqlx::query("INSERT INTO playlists (id, org_id, name, items, shuffle, updated_at, origin) VALUES (?,?,?,?,?,?,'ai')")
            .bind(id).bind(org).bind(pname).bind(items).bind(shuffle).bind(now())
            .execute(&mut *tx).await?;
    }
    tx.commit().await?;
    let regions = match normalize_regions(state, org, &regions).await {
        Ok(r) => r,
        Err(e) => {
            for (id, ..) in &tx_playlists {
                let _ = sqlx::query("DELETE FROM playlists WHERE id = ?").bind(id).execute(&state.db).await;
            }
            return Err(e);
        }
    };

    let layout_id = match revise {
        Some(lid) => {
            let old: Vec<String> = fetch_layout(state, org, lid).await?.to_json()["regions"]
                .as_array().into_iter().flatten()
                .filter_map(|r| r["playlist_id"].as_str().map(str::to_owned)).collect();
            sqlx::query("UPDATE layouts SET name = ?, background = ?, regions = ?, updated_at = ? WHERE id = ? AND org_id = ?")
                .bind(&name).bind(&background).bind(regions.to_string()).bind(now()).bind(lid).bind(org)
                .execute(&state.db).await?;
            // Remove AI playlists the revision replaced, if nothing else uses them.
            for pid in old {
                let used: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM layouts WHERE org_id = ? AND regions LIKE ?")
                    .bind(org).bind(format!("%{pid}%")).fetch_one(&state.db).await?;
                if used == 0 {
                    sqlx::query("DELETE FROM playlists WHERE id = ? AND org_id = ? AND origin = 'ai'")
                        .bind(&pid).bind(org).execute(&state.db).await?;
                }
            }
            lid.to_string()
        }
        None => {
            let id = new_id();
            sqlx::query("INSERT INTO layouts (id, org_id, name, width, height, background, regions, updated_at, origin) VALUES (?,?,?,?,?,?,?,?,'ai')")
                .bind(&id).bind(org).bind(&name).bind(width).bind(height).bind(&background).bind(regions.to_string()).bind(now())
                .execute(&state.db).await?;
            id
        }
    };
    Ok(layout_id)
}
