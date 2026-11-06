//! Qivomedia Player for Windows.
//!
//! `qivo-player.exe`                  supervisor (watchdog): starts and restarts the player
//! `qivo-player.exe --player`         the player window itself
//! `qivo-player.exe --server <url>`   save the server address (mass deployment), then run
//! options: `--windowed` (not fullscreen, for testing), `--no-supervisor` (run the player directly)
//!
//! The window shows the Qivomedia web player from the configured server, so screens get
//! every new feature without reinstalling. The native layer adds kiosk behaviour and the
//! remote controls a browser can't do. Only pages from the configured server may call
//! them, through Tauri's capability system.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod native;
mod supervisor;

use std::time::Duration;

use serde_json::{json, Value};
use tauri::{ipc::CapabilityBuilder, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use config::Config;

/// Permissions the server's player page receives (nothing else, in particular not the
/// setup commands that change which server the screen trusts).
const SERVER_PERMISSIONS: &[&str] = &[
    "allow-device-info",
    "allow-screenshot",
    "allow-heartbeat",
    "allow-reboot",
    "allow-restart-player",
    "allow-display-power",
    "allow-open-setup",
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = args.iter().position(|a| a == "--server") {
        if let Some(url) = args.get(i + 1) {
            match config::normalize_server(url) {
                Ok(origin) => {
                    let mut cfg = Config::load();
                    cfg.server_url = Some(origin.clone());
                    let _ = cfg.save();
                    config::log(&format!("server set to {origin} from the command line"));
                }
                Err(e) => eprintln!("--server: {e}"),
            }
        }
    }
    let windowed = args.iter().any(|a| a == "--windowed");
    if !args.iter().any(|a| a == "--player" || a == "--no-supervisor") {
        let pass: Vec<String> = if windowed { vec!["--windowed".into()] } else { vec![] };
        std::process::exit(supervisor::run(&pass));
    }
    run_player(windowed);
}

// ---------------- commands callable from pages ----------------

#[tauri::command]
fn device_info(sys: tauri::State<'_, native::Sys>) -> Value {
    native::device_info(&sys)
}

#[tauri::command]
async fn screenshot() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(native::screenshot).await.map_err(|e| e.to_string())?
}

#[tauri::command]
fn heartbeat(window: WebviewWindow) {
    config::touch_heartbeat();
    if let Ok(u) = window.url() {
        if !is_local(&u) {
            config::mark_loaded(&u.origin().ascii_serialization());
        }
    }
}

#[tauri::command]
fn reboot() -> Result<(), String> {
    native::reboot()
}

#[tauri::command]
fn restart_player(app: tauri::AppHandle) {
    config::log("restart requested from the dashboard");
    app.exit(supervisor::EXIT_RESTART);
}

#[tauri::command]
fn display_power(on: bool) -> Result<(), String> {
    native::display_power(on)
}

#[tauri::command]
fn open_setup(window: WebviewWindow) -> Result<(), String> {
    window.navigate(local_url("index.html")).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_config() -> Value {
    let c = Config::load();
    json!({ "server_url": c.server_url, "restart_at": c.restart_at, "autostart": c.autostart, "version": env!("CARGO_PKG_VERSION"), "home": config::home() })
}

#[tauri::command]
async fn test_server(url: String) -> Result<Value, String> {
    let (origin, ok) = resolve_server(&url).await?;
    Ok(json!({ "origin": origin, "ok": ok }))
}

/// The first address that answers, or the first candidate when none does (offline install).
async fn resolve_server(input: &str) -> Result<(String, bool), String> {
    let candidates = config::server_candidates(input)?;
    for c in &candidates {
        if server_ok(c).await {
            return Ok((c.clone(), true));
        }
    }
    Ok((candidates[0].clone(), false))
}

/// Save settings and switch to the server's player.
#[tauri::command]
async fn save_config(app: tauri::AppHandle, window: WebviewWindow, server_url: String, restart_at: Option<String>, autostart: Option<bool>) -> Result<String, String> {
    let (origin, _) = resolve_server(&server_url).await?;
    let mut cfg = Config::load();
    cfg.server_url = Some(origin.clone());
    if let Some(r) = restart_at {
        let r = r.trim().to_owned();
        if !r.is_empty() && chrono::NaiveTime::parse_from_str(&r, "%H:%M").is_err() {
            return Err("Daily restart must be a time like 04:00 (or empty for never)".into());
        }
        cfg.restart_at = r;
    }
    if let Some(a) = autostart {
        cfg.autostart = a;
        #[cfg(windows)]
        native::set_autostart(a);
    }
    cfg.save().map_err(|e| format!("could not save settings: {e}"))?;
    config::log(&format!("settings saved: server {origin}"));
    grant_server_access(&app, &origin)?;
    window.navigate(player_url(&origin)?).map_err(|e| e.to_string())?;
    Ok(origin)
}

#[tauri::command]
fn quit_player(app: tauri::AppHandle) {
    config::log("player closed by a technician");
    app.exit(supervisor::EXIT_QUIT);
}

// ---------------- helpers ----------------

fn is_local(u: &url::Url) -> bool {
    u.scheme() == "tauri" || u.host_str() == Some("tauri.localhost")
}

fn local_url(page: &str) -> url::Url {
    let base = if cfg!(windows) { "http://tauri.localhost/" } else { "tauri://localhost/" };
    url::Url::parse(base).unwrap().join(page).unwrap()
}

fn player_url(origin: &str) -> Result<url::Url, String> {
    url::Url::parse(&format!("{origin}/player/")).map_err(|e| e.to_string())
}

async fn server_ok(origin: &str) -> bool {
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(4)).build() else { return false };
    match client.get(format!("{origin}/api/health")).send().await {
        Ok(r) if r.status().is_success() => r.text().await.map(|t| t.trim() == "ok").unwrap_or(false),
        _ => false,
    }
}

/// Let pages from exactly this server origin call the player commands.
fn grant_server_access(app: &tauri::AppHandle, origin: &str) -> Result<(), String> {
    let id = format!("server-{}", origin.replace(|c: char| !c.is_ascii_alphanumeric(), "-"));
    let mut cap = CapabilityBuilder::new(id).remote(format!("{origin}/*")).window("main");
    for p in SERVER_PERMISSIONS {
        cap = cap.permission(*p);
    }
    match app.add_capability(cap) {
        Ok(()) => Ok(()),
        // Already granted earlier in this run.
        Err(e) if e.to_string().contains("already") => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

fn run_player(windowed: bool) {
    config::log(&format!("player: starting (version {})", env!("CARGO_PKG_VERSION")));
    tauri::Builder::default()
        .manage(native::Sys::default())
        .invoke_handler(tauri::generate_handler![
            device_info, screenshot, heartbeat, reboot, restart_player, display_power, open_setup,
            get_config, save_config, test_server, quit_player
        ])
        .setup(move |app| {
            native::keep_awake();
            let cfg = Config::load();
            // Start on the server's player when it answers, or when it has loaded before
            // (its offline cache then plays without a network). Otherwise show setup /
            // "connecting", which keeps retrying.
            let start = match cfg.server_url.as_deref().map(config::normalize_server) {
                Some(Ok(origin)) => {
                    let reachable = tauri::async_runtime::block_on(server_ok(&origin));
                    if reachable || config::loaded_before(&origin) {
                        grant_server_access(app.handle(), &origin)?;
                        WebviewUrl::External(player_url(&origin)?)
                    } else {
                        config::log(&format!("player: {origin} not reachable yet, showing the connecting screen"));
                        WebviewUrl::App("index.html#connecting".into())
                    }
                }
                _ => WebviewUrl::App("index.html".into()),
            };
            #[allow(unused_mut)] // only changed on Windows
            let mut b = WebviewWindowBuilder::new(app, "main", start)
                .title("Qivomedia Player")
                .fullscreen(!windowed)
                .decorations(windowed)
                .always_on_top(!windowed)
                .focused(true)
                .inner_size(1280.0, 720.0)
                .background_color(tauri::window::Color(0, 0, 0, 255))
                // Browser storage (pairing key, offline cache) lives next to config.json:
                // deleting that one folder fully resets the screen.
                .data_directory(config::home().join("webview"));
            #[cfg(windows)]
            {
                // Videos with sound must start without anyone touching the screen.
                // (Tauri's own defaults are repeated because this replaces them.)
                b = b.additional_browser_args(
                    "--autoplay-policy=no-user-gesture-required --disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-pinch --overscroll-history-navigation=0",
                );
            }
            let w = b.build()?;
            let _ = w.set_cursor_visible(windowed);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Qivomedia player");
}
