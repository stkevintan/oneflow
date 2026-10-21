//! Daemon control channel: `GET /control` upgraded to a WebSocket.
//!
//! This endpoint is outside the HTTP contract; its messages are defined in
//! `protocol/src/lib.rs`.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use protocol::{ControlMessage, DaemonMessage};
use tokio::sync::broadcast;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{now_ts, AppState};

pub async fn ws_handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let connection_id = Uuid::new_v4().to_string();
    info!(%connection_id, "Daemon connected via WebSocket");

    state.store.daemon_connected(&connection_id, now_ts()).await;

    // Subscribe before reading the config so an update made in between is
    // delivered rather than lost.
    let mut updates = state.broadcast.subscribe();
    let initial = ControlMessage::from(state.store.config().await);

    if send_control_message(&mut socket, &initial).await.is_ok() {
        loop {
            tokio::select! {
                incoming = socket.recv() => {
                    match incoming {
                        Some(Ok(Message::Text(text))) => {
                            match serde_json::from_str::<DaemonMessage>(&text) {
                                Ok(message) => {
                                    state
                                        .store
                                        .apply_daemon_message(&connection_id, message, now_ts())
                                        .await;
                                }
                                Err(err) => warn!(%connection_id, error = %err, "received invalid daemon message"),
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Err(err)) => {
                            warn!(%connection_id, error = %err, "websocket receive error");
                            break;
                        }
                        _ => {}
                    }
                }
                update = updates.recv() => {
                    match update {
                        Ok(message) => {
                            if send_control_message(&mut socket, &message).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(%connection_id, skipped, "daemon fell behind on updates");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    }

    state.store.daemon_disconnected(&connection_id).await;
    info!(%connection_id, "Daemon disconnected");
}

async fn send_control_message(socket: &mut WebSocket, message: &ControlMessage) -> Result<(), ()> {
    let json = serde_json::to_string(message).map_err(|err| {
        warn!(error = %err, "failed to serialize control message");
    })?;
    socket.send(Message::Text(json)).await.map_err(|err| {
        warn!(error = %err, "failed to send control message");
    })
}
