#![recursion_limit = "512"]
//! Qivomedia — digital signage platform server.
//!
//! One binary serves the REST API, WebSocket push channels, the CMS dashboard,
//! the web player and content-addressed media files.

mod ai;
mod alerts;
mod auth;
mod displays;
mod error;
mod hub;
mod mail;
mod media;
mod platform;
mod player;
mod public;
mod ratelimit;
mod reports;
mod content;
mod schedules;
mod state;
mod triggers;
#[cfg(test)]
mod tests;
mod util;

use std::{net::SocketAddr, path::PathBuf, str::FromStr};

use axum::{
    http::{header, HeaderValue},
    routing::get,
    Router,
};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};
use tracing_subscriber::EnvFilter;

use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "qivo_server=info,tower_http=warn".into()),
        )
        .init();

    let data_dir = PathBuf::from(std::env::var("QIVO_DATA_DIR").unwrap_or_else(|_| "data".into()));
    let web_dir = PathBuf::from(std::env::var("QIVO_WEB_DIR").unwrap_or_else(|_| "web".into()));
    let bind = std::env::var("QIVO_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());

    // Sub-commands used by Docker health checks and the backup script.
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("healthcheck") => return healthcheck(&bind),
        Some("backup") => {
            let dest = args.get(2).cloned().unwrap_or_else(|| {
                data_dir
                    .join("backups")
                    .join(format!("qivo-{}.db", chrono::Utc::now().format("%Y%m%d-%H%M%S")))
                    .display()
                    .to_string()
            });
            return backup(&data_dir, &dest).await;
        }
        Some("make-admin") => {
            let email = args.get(2).ok_or_else(|| anyhow::anyhow!("usage: qivo-server make-admin <email>"))?;
            return make_admin(&data_dir, email).await;
        }
        Some("--version") | Some("version") => {
            println!("qivo-server {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(other) if !other.starts_with('-') => anyhow::bail!("unknown command '{other}' (use: healthcheck | backup [file] | make-admin <email> | version)"),
        _ => {}
    }
    let allow_signup = std::env::var("QIVO_ALLOW_SIGNUP").map(|v| v == "1" || v == "true").unwrap_or(false);

    std::fs::create_dir_all(data_dir.join("media"))?;
    std::fs::create_dir_all(data_dir.join("tmp"))?;

    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| format!("sqlite://{}", data_dir.join("qivo.db").display()));
    let opts = SqliteConnectOptions::from_str(&db_url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(5));
    let db = SqlitePoolOptions::new().max_connections(16).connect_with(opts).await?;
    sqlx::migrate!("./migrations").run(&db).await?;

    let state = AppState::new(db, data_dir.clone(), allow_signup);

    let app = build_app(state.clone(), &web_dir, &data_dir);

    tokio::spawn(hub::presence_sweeper(state.clone()));

    let addr = SocketAddr::from_str(&bind)?;
    tracing::info!("Qivomedia listening on http://{addr}  (dashboard: /, player: /player)");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).with_graceful_shutdown(shutdown()).await?;
    Ok(())
}

/// The whole HTTP application (also used by the in-process test suite).
pub fn build_app(state: AppState, web_dir: &std::path::Path, data_dir: &std::path::Path) -> Router {
    let api = Router::new()
        .merge(auth::routes())
        .merge(media::routes())
        .merge(content::routes())
        .merge(displays::routes())
        .merge(schedules::routes())
        .merge(triggers::routes())
        .merge(reports::routes())
        .merge(player::routes())
        .merge(platform::routes())
        .merge(ai::routes())
        .merge(alerts::routes())
        .merge(public::routes())
        .route("/health", get(|| async { "ok" }));

    let files = Router::new()
        .fallback_service(ServeDir::new(data_dir.join("media")))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        ));

    Router::new()
        .nest("/api", api)
        .route("/ws/player", get(player::ws_handler))
        .route("/ws/admin", get(hub::admin_ws_handler))
        .nest("/files", files)
        .nest_service(
            "/player",
            ServeDir::new(web_dir.join("player")).fallback(ServeFile::new(web_dir.join("player/index.html"))),
        )
        .nest_service("/shared", ServeDir::new(web_dir.join("shared")))
        .fallback_service(ServeDir::new(web_dir.join("app")).fallback(ServeFile::new(web_dir.join("app/index.html"))))
        .layer(SetResponseHeaderLayer::if_not_present(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::if_not_present(header::REFERRER_POLICY, HeaderValue::from_static("strict-origin-when-cross-origin")))
        .layer(SetResponseHeaderLayer::if_not_present(header::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN")))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Exit 0 if the local server answers /api/health (no curl needed in the slim image).
fn healthcheck(bind: &str) -> anyhow::Result<()> {
    use std::io::{Read, Write};
    let port = bind.rsplit(':').next().unwrap_or("8080");
    let mut s = std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse()?,
        std::time::Duration::from_secs(3),
    )?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;
    s.write_all(b"GET /api/health HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
    let mut buf = String::new();
    s.read_to_string(&mut buf)?;
    if buf.starts_with("HTTP/1.0 200") || buf.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        anyhow::bail!("unhealthy: {}", buf.lines().next().unwrap_or(""))
    }
}

/// Consistent online copy of the database (safe while the server is running).
async fn backup(data_dir: &std::path::Path, dest: &str) -> anyhow::Result<()> {
    if let Some(parent) = std::path::Path::new(dest).parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::path::Path::new(dest).exists() {
        anyhow::bail!("{dest} already exists");
    }
    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| format!("sqlite://{}", data_dir.join("qivo.db").display()));
    let opts = SqliteConnectOptions::from_str(&db_url)?.busy_timeout(std::time::Duration::from_secs(30));
    let db = SqlitePoolOptions::new().max_connections(1).connect_with(opts).await?;
    sqlx::query("VACUUM INTO ?").bind(dest).execute(&db).await?;
    println!("database backed up to {dest}");
    Ok(())
}

/// Grant platform-admin rights to an existing user (e.g. a second operator).
async fn make_admin(data_dir: &std::path::Path, email: &str) -> anyhow::Result<()> {
    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| format!("sqlite://{}", data_dir.join("qivo.db").display()));
    let db = SqlitePoolOptions::new().max_connections(1).connect_with(SqliteConnectOptions::from_str(&db_url)?).await?;
    sqlx::migrate!("./migrations").run(&db).await?;
    let r = sqlx::query("UPDATE users SET platform_admin = 1 WHERE email = ?")
        .bind(email.trim().to_lowercase())
        .execute(&db)
        .await?;
    if r.rows_affected() == 0 {
        anyhow::bail!("no user with email {email}");
    }
    println!("{email} is now a platform admin");
    Ok(())
}

async fn shutdown() {
    // Docker sends SIGTERM on stop/upgrade; handle it so shutdown is clean and fast.
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
