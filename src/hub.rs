//! Real-time event hub.
//!
//! Each organization has two channels: one for its players, one for its dashboards.
//! A message is serialized once and shared by every socket. Dashboard-only events
//! such as "screen came online" never wake player sockets, and one organization's
//! events never wake another's. With 10,000 screens connected, a single shared
//! channel meant every connect woke every socket, which costs O(N²).

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock as StdRwLock},
};

use axum::{
    extract::{
        ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::{auth::AuthUser, state::AppState};

/// How many messages a slow socket may fall behind before it's told to re-sync.
const CHANNEL_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
pub enum Audience {
    /// Every player in the org.
    OrgPlayers,
    /// Specific players.
    Displays(Vec<String>),
    /// Every dashboard session in the org.
    Admins,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub org_id: String,
    pub audience: Audience,
    pub payload: Value,
}

impl Event {
    pub fn to_org_players(org: &str, payload: Value) -> Self {
        Self { org_id: org.into(), audience: Audience::OrgPlayers, payload }
    }
    pub fn to_displays(org: &str, ids: Vec<String>, payload: Value) -> Self {
        Self { org_id: org.into(), audience: Audience::Displays(ids), payload }
    }
    pub fn to_admins(org: &str, payload: Value) -> Self {
        Self { org_id: org.into(), audience: Audience::Admins, payload }
    }
}

/// What actually travels on a channel: pre-serialized and cheap to clone.
#[derive(Debug)]
pub struct Packet {
    /// `None` = every player of the org.
    pub only: Option<HashSet<String>>,
    pub text: Utf8Bytes,
}
impl Packet {
    pub fn is_for(&self, display_id: &str) -> bool {
        self.only.as_ref().is_none_or(|s| s.contains(display_id))
    }
}

type Channels = StdRwLock<HashMap<String, broadcast::Sender<Arc<Packet>>>>;

#[derive(Default)]
pub struct Hub {
    players: Channels,
    admins: Channels,
}

fn sender(map: &Channels, org: &str) -> broadcast::Sender<Arc<Packet>> {
    if let Some(s) = map.read().unwrap().get(org) {
        return s.clone();
    }
    map.write().unwrap().entry(org.to_owned()).or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0).clone()
}

impl Hub {
    pub fn publish(&self, ev: Event) {
        let map = match ev.audience {
            Audience::Admins => &self.admins,
            _ => &self.players,
        };
        // Nobody listening → nothing to do (and no channel to create).
        let Some(tx) = map.read().unwrap().get(&ev.org_id).cloned() else { return };
        if tx.receiver_count() == 0 {
            return;
        }
        let only = match ev.audience {
            Audience::Displays(ids) => Some(ids.into_iter().collect()),
            _ => None,
        };
        let _ = tx.send(Arc::new(Packet { only, text: ev.payload.to_string().into() }));
    }
    pub fn subscribe_player(&self, org: &str) -> broadcast::Receiver<Arc<Packet>> {
        sender(&self.players, org).subscribe()
    }
    pub fn subscribe_admin(&self, org: &str) -> broadcast::Receiver<Arc<Packet>> {
        sender(&self.admins, org).subscribe()
    }
    /// Player sockets currently open for an organization.
    pub fn player_sockets(&self, org: &str) -> usize {
        self.players.read().unwrap().get(org).map_or(0, |s| s.receiver_count())
    }
    /// Drop channels nobody listens to any more.
    pub fn prune(&self) {
        for map in [&self.players, &self.admins] {
            map.write().unwrap().retain(|_, s| s.receiver_count() > 0);
        }
    }
}

pub async fn is_online(state: &AppState, display_id: &str) -> bool {
    state.presence.read().await.get(display_id).copied().unwrap_or(0) > 0
}

/// How many of `ids` are online, under a single lock.
pub async fn online_count<'a>(state: &AppState, ids: impl IntoIterator<Item = &'a String>) -> usize {
    let p = state.presence.read().await;
    ids.into_iter().filter(|i| p.get(*i).copied().unwrap_or(0) > 0).count()
}

pub async fn admin_ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>, user: AuthUser) -> Response {
    ws.max_message_size(64 * 1024)
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .on_upgrade(move |socket| admin_socket(socket, state, user))
}

async fn admin_socket(mut socket: WebSocket, state: AppState, user: AuthUser) {
    let org = user.org().to_owned();
    let mut rx = state.hub.subscribe_admin(&org);
    let _ = socket.send(Message::Text(json!({ "type": "hello" }).to_string().into())).await;
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(p) => {
                    if socket.send(Message::Text(p.text.clone())).await.is_err() { break; }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    }
}

/// Housekeeping: forget offline screens, drop idle channels, and write
/// "last seen" times to the database in one batch instead of one write per event.
pub async fn presence_sweeper(state: AppState) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
    loop {
        tick.tick().await;
        state.presence.write().await.retain(|_, n| *n > 0);
        state.hub.prune();
        if let Err(e) = state.flush_seen().await {
            tracing::warn!("could not save last-seen times: {e}");
        }
    }
}
