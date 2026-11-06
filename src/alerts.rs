//! Emergency alerts. An alert takes over its target screens immediately, above every
//! schedule and trigger, and stays until someone clears it. Active alerts are also part
//! of every screen's manifest, so a screen that reboots or reconnects mid-alert shows it.
//!
//! Sources: the dashboard, a JSON webhook, or a CAP (Common Alerting Protocol) XML feed
//! from a campus emergency system.

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    auth::{audit, AuthUser},
    displays::{resolve_targets, validate_target},
    error::{AppError, AppResult},
    hub::Event,
    state::AppState,
    util::{new_id, now, random_token},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/alerts", get(list).post(start))
        .route("/alerts/templates", get(templates))
        .route("/alerts/{id}/clear", post(clear_one))
        .route("/alerts/clear-all", post(clear_all))
        .route("/alerts/hook", get(hook_info))
        .route("/alerts/hook/rotate", post(rotate_hook))
        .route("/alert-hooks/{key}", post(webhook))
}

// ------------------------------------------------------------------ templates

struct Template {
    kind: &'static str,
    color: &'static str,
    lines: [(&'static str, &'static str, &'static str); 3], // (lang, title, message)
}

const TEMPLATES: &[Template] = &[
    Template { kind: "fire", color: "red", lines: [
        ("fr", "Alerte incendie", "Évacuez immédiatement le bâtiment par la sortie la plus proche. N'utilisez pas les ascenseurs."),
        ("ar", "إنذار حريق", "غادروا المبنى فوراً عبر أقرب مخرج. لا تستعملوا المصاعد."),
        ("en", "Fire alarm", "Leave the building now by the nearest exit. Do not use the lifts."),
    ]},
    Template { kind: "evacuation", color: "red", lines: [
        ("fr", "Évacuation", "Quittez le bâtiment dans le calme et rejoignez le point de rassemblement."),
        ("ar", "إخلاء", "غادروا المبنى بهدوء وتوجهوا إلى نقطة التجمع."),
        ("en", "Evacuation", "Leave the building calmly and go to the assembly point."),
    ]},
    Template { kind: "lockdown", color: "red", lines: [
        ("fr", "Confinement", "Restez à l'intérieur, fermez les portes à clé et éloignez-vous des fenêtres. Attendez les instructions."),
        ("ar", "إغلاق أمني", "ابقوا في الداخل، أغلقوا الأبواب بالمفتاح وابتعدوا عن النوافذ. انتظروا التعليمات."),
        ("en", "Lockdown", "Stay inside, lock the doors and keep away from windows. Wait for instructions."),
    ]},
    Template { kind: "weather", color: "amber", lines: [
        ("fr", "Alerte météo", "Conditions dangereuses. Restez à l'intérieur jusqu'à nouvel ordre."),
        ("ar", "تحذير جوي", "ظروف خطيرة. ابقوا في الداخل حتى إشعار آخر."),
        ("en", "Severe weather", "Dangerous conditions. Stay indoors until further notice."),
    ]},
];

fn template(kind: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.kind == kind)
}

fn template_lines(t: &Template) -> Value {
    Value::Array(t.lines.iter().map(|(l, ti, m)| json!({ "lang": l, "title": ti, "message": m })).collect())
}

async fn templates(_user: AuthUser) -> Json<Value> {
    Json(Value::Array(
        TEMPLATES.iter().map(|t| json!({ "kind": t.kind, "color": t.color, "lines": template_lines(t) })).collect(),
    ))
}

// ------------------------------------------------------------------ model

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Alert {
    pub id: String,
    pub kind: String,
    pub color: String,
    pub lines: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub test: bool,
    pub sound: bool,
    pub source: String,
    pub started_by: Option<String>,
    pub started_at: String,
    pub cleared_at: Option<String>,
}

impl Alert {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "kind": self.kind, "color": self.color,
            "lines": serde_json::from_str::<Value>(&self.lines).unwrap_or(json!([])),
            "target_type": self.target_type, "target_id": self.target_id,
            "test": self.test, "sound": self.sound, "source": self.source,
            "started_by": self.started_by, "started_at": self.started_at, "cleared_at": self.cleared_at,
        })
    }
}

const COLS: &str = "id, kind, color, lines, target_type, target_id, test, sound, source, started_by, started_at, cleared_at";

/// Active alerts that apply to one screen (used by the player manifest).
pub async fn active_for_display(state: &AppState, org: &str, display_id: &str, group_id: Option<&str>) -> AppResult<Vec<Value>> {
    let rows: Vec<Alert> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM alerts WHERE org_id = ? AND cleared_at IS NULL
            AND (target_type = 'all' OR (target_type = 'group' AND target_id = ?) OR (target_type = 'display' AND target_id = ?))
          ORDER BY started_at DESC"
    ))
    .bind(org)
    .bind(group_id)
    .bind(display_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows.iter().map(Alert::to_json).collect())
}

// ------------------------------------------------------------------ core actions

struct NewAlert {
    kind: String,
    color: String,
    lines: Value,
    target_type: String,
    target_id: Option<String>,
    test: bool,
    sound: bool,
    source: &'static str,
    external_id: Option<String>,
    started_by: Option<String>,
}

async fn do_start(state: &AppState, org: &str, a: NewAlert) -> AppResult<Value> {
    validate_target(state, org, &a.target_type, a.target_id.as_deref()).await?;
    let id = new_id();
    sqlx::query(
        "INSERT INTO alerts (id, org_id, kind, color, lines, target_type, target_id, test, sound, source, external_id, started_by, started_at)
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&id).bind(org).bind(&a.kind).bind(&a.color).bind(a.lines.to_string())
    .bind(&a.target_type).bind(&a.target_id).bind(a.test).bind(a.sound).bind(a.source)
    .bind(&a.external_id).bind(&a.started_by).bind(now())
    .execute(&state.db)
    .await?;
    let alert: Alert = sqlx::query_as(&format!("SELECT {COLS} FROM alerts WHERE id = ?")).bind(&id).fetch_one(&state.db).await?;
    let j = alert.to_json();
    let screens = resolve_targets(state, org, &a.target_type, a.target_id.as_deref()).await?;
    let n = screens.len();
    // Instant takeover over the socket; the manifest refresh makes it durable.
    state.publish(Event::to_displays(org, screens, json!({ "type": "alert", "alert": j })));
    state.content_changed(org);
    state.publish(Event::to_admins(org, json!({ "type": "alert_started", "alert": j, "screens": n })));
    let title = a.lines[0]["title"].as_str().unwrap_or(&a.kind).to_string();
    audit(state, org, a.started_by.as_deref(), if a.test { "alert.test_started" } else { "alert.started" },
          &format!("{title} → {} ({} screens, via {})", a.target_type, n, a.source)).await;
    Ok(json!({ "alert": j, "screens": n }))
}

async fn do_clear(state: &AppState, org: &str, ids: Vec<String>, by: Option<&str>, via: &str) -> AppResult<usize> {
    let mut cleared = 0;
    for id in ids {
        let r = sqlx::query("UPDATE alerts SET cleared_at = ?, cleared_by = ? WHERE id = ? AND org_id = ? AND cleared_at IS NULL")
            .bind(now()).bind(by).bind(&id).bind(org)
            .execute(&state.db).await?;
        if r.rows_affected() == 1 {
            cleared += 1;
            state.publish(Event::to_org_players(org, json!({ "type": "alert_cleared", "id": id })));
            state.publish(Event::to_admins(org, json!({ "type": "alert_cleared", "id": id })));
        }
    }
    if cleared > 0 {
        state.content_changed(org);
        audit(state, org, by, "alert.cleared", &format!("{cleared} alert(s) via {via}")).await;
    }
    Ok(cleared)
}

async fn active_ids(state: &AppState, org: &str) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT id FROM alerts WHERE org_id = ? AND cleared_at IS NULL").bind(org).fetch_all(&state.db).await?)
}

// ------------------------------------------------------------------ dashboard API

#[derive(Deserialize)]
struct ListQ {
    #[serde(default)]
    active: bool,
}

async fn list(State(state): State<AppState>, user: AuthUser, Query(q): Query<ListQ>) -> AppResult<Json<Vec<Value>>> {
    let sql = if q.active {
        format!("SELECT {COLS} FROM alerts WHERE org_id = ? AND cleared_at IS NULL ORDER BY started_at DESC")
    } else {
        format!("SELECT {COLS} FROM alerts WHERE org_id = ? ORDER BY started_at DESC LIMIT 50")
    };
    let rows: Vec<Alert> = sqlx::query_as(&sql).bind(user.org()).fetch_all(&state.db).await?;
    Ok(Json(rows.iter().map(Alert::to_json).collect()))
}

#[derive(Deserialize)]
struct StartReq {
    kind: String,
    /// For kind=custom.
    title: Option<String>,
    message: Option<String>,
    color: Option<String>,
    #[serde(default = "all")]
    target_type: String,
    target_id: Option<String>,
    #[serde(default)]
    test: bool,
    #[serde(default = "yes")]
    sound: bool,
}
fn all() -> String { "all".into() }
fn yes() -> bool { true }

fn build_new(r: StartReq, source: &'static str, started_by: Option<String>, external_id: Option<String>) -> AppResult<NewAlert> {
    let (color, lines) = if let Some(t) = template(&r.kind) {
        (t.color.to_string(), template_lines(t))
    } else if r.kind == "custom" {
        let title = r.title.as_deref().map(str::trim).filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::BadRequest("a custom alert needs a title".into()))?;
        let message = r.message.as_deref().map(str::trim).unwrap_or("");
        if title.chars().count() > 120 || message.chars().count() > 600 {
            return Err(AppError::BadRequest("title max 120 characters, message max 600".into()));
        }
        let color = r.color.as_deref().filter(|c| ["red", "amber", "blue"].contains(c)).unwrap_or("red");
        (color.to_string(), json!([{ "lang": "custom", "title": title, "message": message }]))
    } else {
        return Err(AppError::BadRequest("kind must be fire, evacuation, lockdown, weather or custom".into()));
    };
    Ok(NewAlert {
        kind: r.kind, color, lines,
        target_type: r.target_type, target_id: r.target_id.filter(|s| !s.is_empty()),
        test: r.test, sound: r.sound, source, external_id, started_by,
    })
}

async fn start(State(state): State<AppState>, user: AuthUser, Json(r): Json<StartReq>) -> AppResult<Json<Value>> {
    // Editors can raise alerts: in an emergency, speed matters more than hierarchy.
    user.require_editor()?;
    let a = build_new(r, "dashboard", Some(user.0.id.clone()), None)?;
    Ok(Json(do_start(&state, user.org(), a).await?))
}

async fn clear_one(State(state): State<AppState>, user: AuthUser, Path(id): Path<String>) -> AppResult<Json<Value>> {
    user.require_editor()?;
    let n = do_clear(&state, user.org(), vec![id], Some(&user.0.id), "dashboard").await?;
    if n == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "cleared": n })))
}

async fn clear_all(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    user.require_editor()?;
    let ids = active_ids(&state, user.org()).await?;
    let n = do_clear(&state, user.org(), ids, Some(&user.0.id), "dashboard").await?;
    Ok(Json(json!({ "cleared": n })))
}

async fn ensure_hook_key(state: &AppState, org: &str) -> AppResult<String> {
    let key: Option<String> = sqlx::query_scalar("SELECT alert_hook_key FROM organizations WHERE id = ?")
        .bind(org).fetch_one(&state.db).await?;
    if let Some(k) = key {
        return Ok(k);
    }
    let k = random_token(40);
    sqlx::query("UPDATE organizations SET alert_hook_key = ? WHERE id = ?").bind(&k).bind(org).execute(&state.db).await?;
    Ok(k)
}

async fn hook_info(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    user.require_admin()?;
    Ok(Json(json!({ "key": ensure_hook_key(&state, user.org()).await? })))
}

async fn rotate_hook(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    user.require_admin()?;
    let k = random_token(40);
    sqlx::query("UPDATE organizations SET alert_hook_key = ? WHERE id = ?").bind(&k).bind(user.org()).execute(&state.db).await?;
    audit(&state, user.org(), Some(&user.0.id), "alert.hook_rotated", "").await;
    Ok(Json(json!({ "key": k })))
}

// ------------------------------------------------------------------ webhook + CAP

/// Body is either JSON `{action: "start"|"clear", kind, title, message, color, target_type, target_id, test}`
/// or a CAP 1.2 XML alert (msgType Alert/Update starts, Cancel clears).
async fn webhook(State(state): State<AppState>, Path(key): Path<String>, headers: HeaderMap, body: String) -> AppResult<Json<Value>> {
    if !crate::ratelimit::allow(&format!("alerthook:{key}"), 60, crate::ratelimit::MIN) {
        return Err(AppError::TooMany("too many alert requests — max 60 per minute".into()));
    }
    let org: Option<String> = sqlx::query_scalar("SELECT id FROM organizations WHERE alert_hook_key = ? AND suspended = 0")
        .bind(&key).fetch_optional(&state.db).await?;
    let org = org.ok_or(AppError::NotFound)?;
    let ct = headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("");
    if ct.contains("xml") || body.trim_start().starts_with('<') {
        return cap(&state, &org, &body).await;
    }
    let v: Value = serde_json::from_str(&body)?;
    if v["action"] == "clear" {
        let ids = active_ids(&state, &org).await?;
        return Ok(Json(json!({ "cleared": do_clear(&state, &org, ids, None, "webhook").await? })));
    }
    let r: StartReq = serde_json::from_value(v)?;
    let a = build_new(r, "webhook", None, None)?;
    Ok(Json(do_start(&state, &org, a).await?))
}

/// Text of the first `<name>` / `<prefix:name>` element (CAP is shallow; no full XML parser needed).
fn xml_text(xml: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(pos) = xml[from..].find(name) {
        let start = from + pos;
        let before = xml[..start].chars().last();
        let after = xml[start + name.len()..].chars().next();
        if matches!(before, Some('<') | Some(':')) && matches!(after, Some('>') | Some(' ')) {
            let open_end = start + xml[start..].find('>')? + 1;
            let close = xml[open_end..].find("</")? + open_end;
            let raw = xml[open_end..close].trim();
            let raw = raw.strip_prefix("<![CDATA[").and_then(|s| s.strip_suffix("]]>")).unwrap_or(raw);
            let text = raw.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&");
            return Some(text.trim().to_string());
        }
        from = start + name.len();
    }
    None
}

async fn cap(state: &AppState, org: &str, xml: &str) -> AppResult<Json<Value>> {
    if xml.len() > 200_000 {
        return Err(AppError::BadRequest("CAP message too large".into()));
    }
    let identifier = xml_text(xml, "identifier");
    let msg_type = xml_text(xml, "msgType").unwrap_or_else(|| "Alert".into());
    if msg_type.eq_ignore_ascii_case("Cancel") {
        // Clear the alerts this feed started (matching references or identifier), else all from CAP.
        let refs = xml_text(xml, "references").unwrap_or_default();
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT id, external_id FROM alerts WHERE org_id = ? AND cleared_at IS NULL AND source = 'cap'",
        ).bind(org).fetch_all(&state.db).await?;
        let matched: Vec<String> = rows.iter()
            .filter(|(_, ext)| ext.as_deref().map(|e| !e.is_empty() && refs.contains(e)).unwrap_or(false))
            .map(|(id, _)| id.clone()).collect();
        let ids = if matched.is_empty() { rows.into_iter().map(|(id, _)| id).collect() } else { matched };
        return Ok(Json(json!({ "cleared": do_clear(state, org, ids, None, "CAP").await? })));
    }
    let status = xml_text(xml, "status").unwrap_or_default();
    let event = xml_text(xml, "event").unwrap_or_else(|| "Alert".into());
    let headline = xml_text(xml, "headline");
    let description = xml_text(xml, "description").unwrap_or_default();
    let instruction = xml_text(xml, "instruction").unwrap_or_default();
    let severity = xml_text(xml, "severity").unwrap_or_default();
    let category = xml_text(xml, "category").unwrap_or_default().to_lowercase();
    let color = match severity.as_str() {
        "Extreme" | "Severe" => "red",
        "Moderate" => "amber",
        _ => "blue",
    };
    let kind = if category == "fire" { "fire" } else if category == "met" { "weather" } else if category == "security" { "lockdown" } else { "custom" };
    let message = [description, instruction].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" ");
    let a = NewAlert {
        kind: kind.into(),
        color: color.into(),
        lines: json!([{ "lang": "custom", "title": headline.unwrap_or(event).chars().take(120).collect::<String>(), "message": message.chars().take(600).collect::<String>() }]),
        target_type: "all".into(),
        target_id: None,
        test: status.eq_ignore_ascii_case("Exercise") || status.eq_ignore_ascii_case("Test"),
        sound: true,
        source: "cap",
        external_id: identifier,
        started_by: None,
    };
    Ok(Json(do_start(state, org, a).await?))
}

#[cfg(test)]
mod tests {
    use super::xml_text;
    #[test]
    fn cap_fields() {
        let x = r#"<?xml version="1.0"?><cap:alert xmlns:cap="urn:oasis:names:tc:emergency:cap:1.2">
          <cap:identifier>CAMPUS-42</cap:identifier><cap:msgType>Alert</cap:msgType>
          <cap:info><cap:event>Fire</cap:event><cap:headline>Fire in Building B</cap:headline>
          <cap:description><![CDATA[Smoke on floor 2 & 3]]></cap:description></cap:info></cap:alert>"#;
        assert_eq!(xml_text(x, "identifier").as_deref(), Some("CAMPUS-42"));
        assert_eq!(xml_text(x, "headline").as_deref(), Some("Fire in Building B"));
        assert_eq!(xml_text(x, "description").as_deref(), Some("Smoke on floor 2 & 3"));
        assert_eq!(xml_text(x, "msgType").as_deref(), Some("Alert"));
        assert_eq!(xml_text(x, "references"), None);
        let plain = "<alert><info><event>Storm</event></info></alert>";
        assert_eq!(xml_text(plain, "event").as_deref(), Some("Storm"));
    }
}
