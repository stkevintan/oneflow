//! Daemon control channel: `GET /control` upgraded to a WebSocket.
//!
//! This endpoint is outside the HTTP contract; its messages are defined in
//! `protocol/src/lib.rs`.

use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use protocol::{ControlMessage, DaemonMessage, VirtualKeyPolicySnapshot};
use tokio::sync::broadcast;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{constant_time_token_eq, now_ts, AppState};

const VIRTUAL_KEY_CAPABILITY: &str = "virtual-keys-v1";

pub async fn ws_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let daemon_id = if let Some(mode) = &state.virtual_key_mode {
        let identity = headers
            .get("x-oneflow-daemon-id")
            .and_then(|value| value.to_str().ok());
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        let Some(identity) = identity else {
            return unauthorized();
        };
        let Some(token) = token.filter(|token| {
            !token.is_empty() && !token.bytes().any(|byte| byte.is_ascii_whitespace())
        }) else {
            return unauthorized();
        };
        let Some(expected) = mode.daemon_tokens.get(identity) else {
            return unauthorized();
        };
        if !constant_time_token_eq(expected, token) {
            return unauthorized();
        }
        Some(identity.to_string())
    } else {
        None
    };

    ws.on_upgrade(move |socket| handle_socket(socket, state, daemon_id))
        .into_response()
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        "unauthorized",
    )
        .into_response()
}

async fn handle_socket(mut socket: WebSocket, state: AppState, daemon_identity: Option<String>) {
    let connection_id = Uuid::new_v4().to_string();
    info!(%connection_id, "Daemon connected via WebSocket");

    state.store.daemon_connected(&connection_id, now_ts()).await;

    // Subscribe before reading state so updates cannot be lost during setup.
    let mut updates = state.broadcast.subscribe();
    let mut registered = daemon_identity.is_none();
    let mut latest_snapshot: Option<VirtualKeyPolicySnapshot> = None;
    let mut refresh = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(60),
        Duration::from_secs(60),
    );
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    if registered {
        let initial = ControlMessage::from(state.store.config().await);
        if send_control_message(&mut socket, &initial).await.is_err() {
            state.store.daemon_disconnected(&connection_id).await;
            return;
        }
    }

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<DaemonMessage>(&text) {
                            Ok(DaemonMessage::Registration(registration)) => {
                                if let Some(expected_id) = daemon_identity.as_deref() {
                                    if registration.daemon_id != expected_id
                                        || !registration.capabilities.iter().any(|capability| capability == VIRTUAL_KEY_CAPABILITY)
                                    {
                                        warn!(%connection_id, "rejected virtual-key daemon registration");
                                        break;
                                    }
                                }
                                state
                                    .store
                                    .apply_daemon_message(
                                        &connection_id,
                                        DaemonMessage::Registration(registration),
                                        now_ts(),
                                    )
                                    .await;
                                if !registered {
                                    registered = true;
                                    let config = match &state.virtual_key_mode {
                                        Some(mode) => match mode.store.config().await {
                                            Ok(config) => {
                                                state.store.set_config(config.clone()).await;
                                                config
                                            }
                                            Err(err) => {
                                                warn!(%connection_id, error = %err, "could not load durable model configuration");
                                                break;
                                            }
                                        },
                                        None => state.store.config().await,
                                    };
                                    let config = ControlMessage::from(config);
                                    if send_control_message(&mut socket, &config).await.is_err() {
                                        break;
                                    }
                                    if let Some(mode) = &state.virtual_key_mode {
                                        match mode.store.snapshot(now_ts()).await {
                                            Ok(snapshot) => {
                                                if send_policy_snapshot(&mut socket, &snapshot).await.is_err() {
                                                    break;
                                                }
                                                latest_snapshot = Some(snapshot);
                                            }
                                            Err(err) => {
                                                warn!(%connection_id, error = %err, "could not load initial virtual-key policy snapshot");
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(message) => {
                                if !registered {
                                    continue;
                                }
                                if let Some(expected_id) = daemon_identity.as_deref() {
                                    let claimed_id = match &message {
                                        DaemonMessage::Heartbeat(heartbeat) => Some(heartbeat.daemon_id.as_str()),
                                        DaemonMessage::Telemetry(telemetry) => Some(telemetry.daemon_id.as_str()),
                                        DaemonMessage::Registration(_) | DaemonMessage::VirtualKeyPolicyAck(_) => None,
                                    };
                                    if claimed_id.is_some_and(|claimed_id| claimed_id != expected_id) {
                                        warn!(%connection_id, "rejected daemon message with mismatched identity");
                                        break;
                                    }
                                }
                                if let DaemonMessage::VirtualKeyPolicyAck(ack) = &message {
                                    let is_current = ack.protocol_version == 1
                                        && latest_snapshot.as_ref().is_some_and(|snapshot| {
                                            ack.revision == snapshot.revision
                                                && ack.valid_until == snapshot.valid_until
                                        });
                                    if !is_current {
                                        warn!(%connection_id, "ignored virtual-key policy acknowledgement not matching the latest snapshot");
                                        continue;
                                    }
                                }
                                state.store.apply_daemon_message(&connection_id, message, now_ts()).await;
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
                        if !registered {
                            continue;
                        }
                        if let ControlMessage::VirtualKeyPolicySnapshot(snapshot) = &message {
                            if state.virtual_key_mode.is_none() {
                                continue;
                            }
                            latest_snapshot = Some(snapshot.clone());
                        }
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
            _ = refresh.tick() => {
                if registered {
                    if let Some(mode) = &state.virtual_key_mode {
                        match mode.store.snapshot(now_ts()).await {
                            Ok(snapshot) => {
                                if send_policy_snapshot(&mut socket, &snapshot).await.is_err() {
                                    break;
                                }
                                latest_snapshot = Some(snapshot);
                            }
                            Err(err) => {
                                warn!(%connection_id, error = %err, "failed to refresh virtual-key policy snapshot");
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    state.store.daemon_disconnected(&connection_id).await;
    info!(%connection_id, "Daemon disconnected");
}

async fn send_policy_snapshot(
    socket: &mut WebSocket,
    snapshot: &VirtualKeyPolicySnapshot,
) -> Result<(), ()> {
    send_control_message(
        socket,
        &ControlMessage::VirtualKeyPolicySnapshot(snapshot.clone()),
    )
    .await
}

async fn send_control_message(socket: &mut WebSocket, message: &ControlMessage) -> Result<(), ()> {
    let json = serde_json::to_string(message).map_err(|err| {
        warn!(error = %err, "failed to serialize control message");
    })?;
    socket.send(Message::Text(json)).await.map_err(|err| {
        warn!(error = %err, "failed to send control message");
    })
}
