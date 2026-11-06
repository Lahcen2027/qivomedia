use std::{collections::HashMap, path::PathBuf, sync::Arc};

use sqlx::SqlitePool;
use tokio::sync::{broadcast, RwLock};

use crate::hub::Event;

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub data_dir: PathBuf,
    pub allow_signup: bool,
    /// Fan-out bus for every real-time event (players + dashboards filter by org/target).
    pub bus: broadcast::Sender<Event>,
    /// display_id -> number of live player sockets.
    pub presence: Arc<RwLock<HashMap<String, usize>>>,
}

impl AppState {
    pub fn new(db: SqlitePool, data_dir: PathBuf, allow_signup: bool) -> Self {
        let (bus, _) = broadcast::channel(1024);
        Self { db, data_dir, allow_signup, bus, presence: Default::default() }
    }

    /// Notify every player of an organization that its content may have changed.
    pub fn content_changed(&self, org_id: &str) {
        self.publish(Event::to_org_players(org_id, serde_json::json!({ "type": "refresh" })));
    }

    pub fn publish(&self, ev: Event) {
        let _ = self.bus.send(ev);
    }
}
