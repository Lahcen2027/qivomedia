use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
};

use serde_json::Value;
use sqlx::SqlitePool;
use tokio::sync::{OnceCell, RwLock};

use crate::hub::{Event, Hub};

/// Layouts + playlists + media for a set of layout ids (the heavy part of a manifest).
pub type Bundle = (Value, Value, Value);
type BundleCell = Arc<OnceCell<Arc<Bundle>>>;

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub data_dir: PathBuf,
    pub allow_signup: bool,
    /// Per-organization real-time channels for players and dashboards.
    pub hub: Arc<Hub>,
    /// display_id -> number of live player sockets.
    pub presence: Arc<RwLock<HashMap<String, usize>>>,
    /// Manifest bundles shared by every screen showing the same layouts. Emptied for an
    /// organization whenever its content changes. When thousands of screens re-download
    /// at once, the database is read once instead of thousands of times.
    bundles: Arc<StdMutex<HashMap<(String, String), BundleCell>>>,
    /// display_id -> newest player info (if any) waiting to be saved with "last seen".
    seen: Arc<StdMutex<HashMap<String, Option<String>>>>,
}

impl AppState {
    pub fn new(db: SqlitePool, data_dir: PathBuf, allow_signup: bool) -> Self {
        if let Err(e) = crate::crypto::init(&data_dir) {
            tracing::error!("encryption key: {e}");
        }
        Self {
            db,
            data_dir,
            allow_signup,
            hub: Default::default(),
            presence: Default::default(),
            bundles: Default::default(),
            seen: Default::default(),
        }
    }

    /// Notify every player of an organization that its content may have changed.
    ///
    /// Players wait a random moment within `spread_ms` before re-downloading, so a
    /// campus of 10,000 screens doesn't hit the server in the same millisecond.
    pub fn content_changed(&self, org_id: &str) {
        self.bundles.lock().unwrap().retain(|(org, _), _| org != org_id);
        let spread_ms = (self.hub.player_sockets(org_id) / 2).min(10_000);
        self.publish(Event::to_org_players(org_id, serde_json::json!({ "type": "refresh", "spread_ms": spread_ms })));
    }

    pub fn publish(&self, ev: Event) {
        self.hub.publish(ev);
    }

    /// Cached bundle for `layout_ids`, computed once even if many screens ask at the same time.
    pub async fn bundle<F, Fut>(&self, org: &str, layout_ids: &BTreeSet<String>, compute: F) -> crate::error::AppResult<Arc<Bundle>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = crate::error::AppResult<Bundle>>,
    {
        let key = (org.to_owned(), layout_ids.iter().cloned().collect::<Vec<_>>().join(","));
        let cell = self.bundles.lock().unwrap().entry(key).or_default().clone();
        cell.get_or_try_init(|| async { compute().await.map(Arc::new) }).await.cloned()
    }

    /// Remember that a screen was seen (optionally with new player info); saved in batches.
    pub fn touch(&self, display_id: &str, info: Option<String>) {
        let mut seen = self.seen.lock().unwrap();
        let slot = seen.entry(display_id.to_owned()).or_default();
        if info.is_some() {
            *slot = info;
        }
    }

    /// Write pending "last seen" times (and player info) in one transaction.
    pub async fn flush_seen(&self) -> sqlx::Result<()> {
        let batch: Vec<(String, Option<String>)> = std::mem::take(&mut *self.seen.lock().unwrap()).into_iter().collect();
        if batch.is_empty() {
            return Ok(());
        }
        let now = crate::util::now();
        let mut tx = self.db.begin().await?;
        for (id, info) in &batch {
            match info {
                Some(i) => sqlx::query("UPDATE displays SET last_seen = ?, player_info = ? WHERE id = ?").bind(&now).bind(i).bind(id),
                None => sqlx::query("UPDATE displays SET last_seen = ? WHERE id = ?").bind(&now).bind(id),
            }
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }
}
