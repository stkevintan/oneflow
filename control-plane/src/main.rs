mod assets;

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router,
};
use protocol::{
    ConfigUpdate, ControlMessage, DaemonMessage, DaemonRecord, ErrorResponse, ModelConfig,
    ModelConfigParameters, ServerInfo, StatusResponse, TelemetryRecord,
};
use tokio::sync::{broadcast, RwLock};
use tracing::{info, warn};
use uuid::Uuid;

const TELEMETRY_HISTORY_LIMIT: usize = 200;

fn default_config() -> ConfigUpdate {
    ConfigUpdate {
        version: "poc-v1".to_string(),
        models: vec![
            ModelConfig {
                model_name: "deepseek-v4-pro".to_string(),
                provider: "custom_openai".to_string(),
                api_base: "https://api.deepseek.com/v1".to_string(),
                parameters: ModelConfigParameters::new(),
            },
            ModelConfig {
                model_name: "deepseek-v4-flash".to_string(),
                provider: "custom_openai".to_string(),
                api_base: "https://api.deepseek.com/v1".to_string(),
                parameters: ModelConfigParameters::new(),
            },
        ],
    }
}

struct SharedState {
    config: ConfigUpdate,
    daemons: BTreeMap<String, DaemonRecord>,
    telemetry: VecDeque<TelemetryRecord>,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            config: default_config(),
            daemons: BTreeMap::new(),
            telemetry: VecDeque::new(),
        }
    }
}

#[derive(Clone)]
struct AppState {
    shared: Arc<RwLock<SharedState>>,
    broadcast: broadcast::Sender<ControlMessage>,
    started_at: i64,
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn new_daemon_record(connection_id: String) -> DaemonRecord {
    let now = now_ts();
    DaemonRecord {
        connection_id,
        daemon_id: None,
        cluster_name: None,
        version: None,
        hostname: None,
        capabilities: Vec::new(),
        connected_at: now,
        last_seen: now,
        active_requests: 0,
        loaded_models: Vec::new(),
        total_requests: 0,
        failed_requests: 0,
        total_prompt_tokens: 0,
        total_completion_tokens: 0,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let config = config_provider::ConfigProvider::load()?;
    let addr: std::net::SocketAddr = config.control_plane.bind_addr.parse()?;
    let (broadcast, _) = broadcast::channel(32);
    let state = AppState {
        shared: Arc::new(RwLock::new(SharedState::default())),
        broadcast,
        started_at: now_ts(),
    };

    let app = create_app(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("🚀 Control Plane POC running on http://{}", addr);
    info!("   Dashboard:          http://{}/", addr);
    info!("   OpenAPI contract:   contract/openapi.yaml");
    info!("   WebSocket endpoint: ws://{}/control", addr);

    axum::serve(listener, app).await?;
    Ok(())
}

fn create_app(state: AppState) -> Router {
    Router::new()
        .route("/control", get(ws_handler))
        .route("/api/status", get(status_handler))
        .route("/api/config", post(config_handler))
        // Unknown API paths must stay machine-readable; only non-API paths fall
        // through to the dashboard shell.
        .route("/api/*path", any(api_not_found))
        .fallback(assets::static_handler)
        .with_state(state)
}

async fn api_not_found(uri: Uri) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: format!("no API route matches {}", uri.path()),
        }),
    )
        .into_response()
}

async fn status_handler(State(state): State<AppState>) -> Json<StatusResponse> {
    let shared = state.shared.read().await;
    Json(StatusResponse {
        server: ServerInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_seconds: now_ts() - state.started_at,
            connected_daemons: shared.daemons.len() as i64,
            config_version: shared.config.version.clone(),
        },
        daemons: shared.daemons.values().cloned().collect(),
        telemetry: shared.telemetry.iter().cloned().collect(),
        config: shared.config.clone(),
    })
}

async fn config_handler(
    State(state): State<AppState>,
    Json(config): Json<ConfigUpdate>,
) -> Response {
    if let Err(message) = validate_config(&config) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: message }),
        )
            .into_response();
    }

    {
        let mut shared = state.shared.write().await;
        shared.config = config.clone();
    }

    let receivers = state
        .broadcast
        .send(ControlMessage::from(config.clone()))
        .unwrap_or(0);
    info!(
        version = %config.version,
        models = config.models.len(),
        receivers,
        "broadcasted configuration update"
    );

    Json(config).into_response()
}

fn validate_config(config: &ConfigUpdate) -> Result<(), String> {
    if config.version.trim().is_empty() {
        return Err("version must not be empty".to_string());
    }
    if config.models.is_empty() {
        return Err("at least one model is required".to_string());
    }
    for model in &config.models {
        if model.model_name.trim().is_empty() {
            return Err("every model requires a model_name".to_string());
        }
        if model.api_base.trim().is_empty() {
            return Err(format!("model '{}' requires an api_base", model.model_name));
        }
    }
    Ok(())
}

async fn ws_handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let connection_id = Uuid::new_v4().to_string();
    info!(%connection_id, "Daemon connected via WebSocket");

    state.shared.write().await.daemons.insert(
        connection_id.clone(),
        new_daemon_record(connection_id.clone()),
    );

    let initial = {
        let shared = state.shared.read().await;
        ControlMessage::from(shared.config.clone())
    };

    let mut updates = state.broadcast.subscribe();

    if send_control_message(&mut socket, &initial).await.is_ok() {
        loop {
            tokio::select! {
                incoming = socket.recv() => {
                    match incoming {
                        Some(Ok(Message::Text(text))) => {
                            handle_daemon_message(&text, &connection_id, &state).await;
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

    state.shared.write().await.daemons.remove(&connection_id);
    info!(%connection_id, "Daemon disconnected");
}

async fn handle_daemon_message(text: &str, connection_id: &str, state: &AppState) {
    let message = match serde_json::from_str::<DaemonMessage>(text) {
        Ok(message) => message,
        Err(err) => {
            warn!(error = %err, "received invalid daemon message");
            return;
        }
    };

    let now = now_ts();
    let mut shared = state.shared.write().await;

    let Some(record) = shared.daemons.get_mut(connection_id) else {
        return;
    };
    record.last_seen = now;

    match message {
        DaemonMessage::Registration(reg) => {
            info!(
                daemon_id = %reg.daemon_id,
                cluster = %reg.cluster_name,
                "Daemon registered"
            );
            record.daemon_id = Some(reg.daemon_id);
            record.cluster_name = Some(reg.cluster_name);
            record.version = Some(reg.version);
            record.hostname = Some(reg.hostname);
            record.capabilities = reg.capabilities;
        }
        DaemonMessage::Heartbeat(heartbeat) => {
            record.active_requests = i64::from(heartbeat.active_requests);
            record.loaded_models = heartbeat.loaded_models;
        }
        DaemonMessage::Telemetry(telemetry) => {
            record.total_requests += 1;
            record.total_prompt_tokens += i64::from(telemetry.prompt_tokens);
            record.total_completion_tokens += i64::from(telemetry.completion_tokens);
            if !telemetry.success {
                record.failed_requests += 1;
            }

            let mut event = TelemetryRecord::from(telemetry);
            event.received_at = now;
            shared.telemetry.push_back(event);
            while shared.telemetry.len() > TELEMETRY_HISTORY_LIMIT {
                shared.telemetry.pop_front();
            }
        }
    }
}

async fn send_control_message(socket: &mut WebSocket, message: &ControlMessage) -> Result<(), ()> {
    let json = serde_json::to_string(message).map_err(|err| {
        warn!(error = %err, "failed to serialize control message");
    })?;
    socket
        .send(Message::Text(json.into()))
        .await
        .map_err(|err| {
            warn!(error = %err, "failed to send control message");
        })
}

/// Contract-first drift guard.
///
/// `contract/openapi.yaml` is the source of truth and is authored before the
/// handlers. The generated DTOs in `protocol/src/generated` come from it, and these
/// tests execute real requests against the axum router, so renaming a route in
/// Rust without updating the contract (or the reverse) fails `cargo test`.
#[cfg(test)]
mod contract_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use serde_json::json;
    use std::str::FromStr;
    use tower::ServiceExt;

    fn contract() -> serde_json::Value {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../contract/openapi.yaml");
        let raw = std::fs::read_to_string(path)
            .expect("contract/openapi.yaml should be readable from the crate root");
        serde_yaml::from_str(&raw).expect("contract/openapi.yaml should be valid YAML")
    }

    fn test_app() -> Router {
        let (broadcast, _) = broadcast::channel(8);
        create_app(AppState {
            shared: Arc::new(RwLock::new(SharedState::default())),
            broadcast,
            started_at: now_ts(),
        })
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");
        serde_json::from_slice(&bytes).expect("response body should be JSON")
    }

    fn request(method: &str, path: &str, body: Option<&str>) -> Request<Body> {
        let builder = Request::builder()
            .method(Method::from_str(&method.to_ascii_uppercase()).expect("valid HTTP method"))
            .uri(path)
            .header("content-type", "application/json");
        match body {
            Some(body) => builder.body(Body::from(body.to_string())),
            None => builder.body(Body::empty()),
        }
        .expect("request should build")
    }

    #[tokio::test]
    async fn every_contract_operation_is_routed() {
        let contract = contract();
        let paths = contract["paths"]
            .as_object()
            .expect("contract should declare paths");

        for (path, item) in paths {
            let item = item.as_object().expect("path item should be an object");
            for method in item.keys() {
                if !matches!(method.as_str(), "get" | "post" | "put" | "patch" | "delete") {
                    continue;
                }

                let response = test_app()
                    .oneshot(request(method, path, None))
                    .await
                    .expect("router should respond");

                assert_ne!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "contract declares {method} {path} but the router does not serve it"
                );
                assert_ne!(
                    response.status(),
                    StatusCode::METHOD_NOT_ALLOWED,
                    "contract declares {method} {path} but the router rejects that method"
                );
            }
        }
    }

    #[tokio::test]
    async fn status_response_exposes_contract_required_fields() {
        let required = contract()["components"]["schemas"]["StatusResponse"]["required"]
            .as_array()
            .expect("StatusResponse.required should exist")
            .iter()
            .map(|field| field.as_str().expect("field name").to_string())
            .collect::<Vec<_>>();

        let response = test_app()
            .oneshot(request("get", "/api/status", None))
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response).await;
        for field in required {
            assert!(
                body.get(&field).is_some(),
                "/api/status response is missing the contract field `{field}`"
            );
        }
    }

    /// The contract declares `daemon_id` and friends as `type: [string, "null"]`,
    /// so an unregistered daemon must serialize explicit nulls rather than
    /// omitting the keys.
    #[test]
    fn nullable_daemon_fields_serialize_as_present_nulls() {
        let record = new_daemon_record("conn-1".to_string());
        let value = serde_json::to_value(&record).expect("daemon record should serialize");

        for field in ["daemon_id", "cluster_name", "version", "hostname"] {
            assert!(
                value.get(field).is_some(),
                "`{field}` must be present in the payload"
            );
            assert!(value[field].is_null(), "`{field}` should serialize as null");
        }
    }

    #[tokio::test]
    async fn config_push_round_trips_and_rejects_invalid_payloads() {
        let app = test_app();
        let payload = json!({
            "version": "contract-test",
            "models": [{
                "model_name": "test-model",
                "provider": "custom_openai",
                "api_base": "https://example.test/v1",
                "parameters": { "upstream_model": "test-upstream" },
            }],
        })
        .to_string();

        let response = app
            .clone()
            .oneshot(request("post", "/api/config", Some(&payload)))
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::OK);

        let status = body_json(
            app.clone()
                .oneshot(request("get", "/api/status", None))
                .await
                .expect("router should respond"),
        )
        .await;
        assert_eq!(status["config"]["version"], "contract-test");
        assert_eq!(status["server"]["config_version"], "contract-test");
        assert_eq!(
            status["config"]["models"][0]["parameters"]["upstream_model"],
            "test-upstream"
        );

        let invalid = json!({ "version": "", "models": [] }).to_string();
        let response = app
            .oneshot(request("post", "/api/config", Some(&invalid)))
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(response).await["error"].is_string());
    }

    #[tokio::test]
    async fn undeclared_api_paths_are_json_404() {
        let response = test_app()
            .oneshot(request("get", "/api/not-in-the-contract", None))
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(body_json(response).await["error"].is_string());
    }
}
