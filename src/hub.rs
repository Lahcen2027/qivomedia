//! Real-time event bus: players and dashboards share one broadcast channel and
//! each socket filters for the events addressed to it.

use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, State},
    response::Response,
};
use serde_json::{json, Value};

use crate::{auth::AuthUser, state::AppState};

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
    pub fn is_for_player(&self, org: &str, display_id: &str) -> bool {
        self.org_id == org
            && match &self.audience {
                Audience::OrgPlayers => true,
                Audience::Displays(ids) => ids.iter().any(|i| i == display_id),
                Audience::Admins => false,
            }
    }
}

pub async fn is_online(state: &AppState, display_id: &str) -> bool {
    state.presence.read().await.get(display_id).copied().unwrap_or(0) > 0
}

pub async fn admin_ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>, user: AuthUser) -> Response {
    ws.on_upgrade(move |socket| admin_socket(socket, state, user))
}

async fn admin_socket(mut socket: WebSocket, state: AppState, user: AuthUser) {
    let mut rx = state.bus.subscribe();
    let org = user.org().to_owned();
    let _ = socket.send(Message::Text(json!({ "type": "hello" }).to_string().into())).await;
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) if ev.org_id == org && matches!(ev.audience, Audience::Admins) => {
                    if socket.send(Message::Text(ev.payload.to_string().into())).await.is_err() { break; }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    }
}

/// Periodically tell dashboards the live player count (cheap liveness signal).
pub async fn presence_sweeper(state: AppState) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        tick.tick().await;
        let mut p = state.presence.write().await;
        p.retain(|_, n| *n > 0);
    }
}
