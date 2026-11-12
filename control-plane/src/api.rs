//! `/v1` management API, implemented against `contract/openapi.yaml`.

use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Path, Query, Request, State,
    },
    http::{header, HeaderValue, Method, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use protocol::{
    ConfigHistoryEntry, ConfigHistoryPage, ControlMessage, Daemon, DaemonList, ErrorResponse,
    HealthResponse, ModelConfig, ModelConfigSet, RollbackConfigRequest, StatusResponse,
    UsageEventPage,
};
use serde::Deserialize;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::info;

use crate::{
    constant_time_token_eq, control, now_ts,
    store::{UsageQuery, USAGE_HISTORY_LIMIT},
    virtual_keys::{ChangeOutcome, ModelChange, RollbackOutcome},
    AppState,
};

const DEFAULT_USAGE_PAGE_SIZE: usize = 50;
const DEFAULT_HISTORY_PAGE_SIZE: i64 = 20;

pub fn create_app(state: AppState, cors_allowed_origins: Vec<HeaderValue>) -> Router {
    let router = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/status", get(status))
        .route("/v1/config", get(get_config).put(put_config))
        .route("/v1/config/history", get(list_config_history))
        .route(
            "/v1/models/:model_name",
            get(get_model).put(put_model).delete(delete_model),
        )
        .route("/v1/config/history/:id/rollback", post(rollback_config))
        .route(
            "/v1/daemons",
            get(list_daemons).post(crate::daemon_credentials::create),
        )
        .route(
            "/v1/daemons/credentials",
            get(crate::daemon_credentials::list),
        )
        .route(
            "/v1/daemons/:connection_id/rotate",
            post(crate::daemon_credentials::rotate),
        )
        .route(
            "/v1/daemons/:connection_id/revoke",
            post(crate::daemon_credentials::revoke),
        )
        .route("/v1/daemons/:connection_id", get(get_daemon))
        .route("/v1/usage/events", get(list_usage_events))
        .route("/key/generate", post(crate::key_api::generate))
        .route("/key/info", get(crate::key_api::info))
        .route("/key/list", get(crate::key_api::list))
        .route("/key/delete", post(crate::key_api::delete))
        .route("/control", get(control::ws_handler))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, authorize_management));

    if cors_allowed_origins.is_empty() {
        return router;
    }
    router.layer(
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(cors_allowed_origins))
            .allow_methods([Method::GET, Method::PUT, Method::POST])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
    )
}

async fn authorize_management(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let is_virtual_key_endpoint = path.starts_with("/key/") || path.starts_with("/v1/daemons");
    let mode = &state.access;
    if path == "/control" {
        let identity = request
            .headers()
            .get("x-oneflow-daemon-id")
            .and_then(|value| value.to_str().ok());
        let token = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .filter(|token| {
                !token.is_empty() && !token.bytes().any(|byte| byte.is_ascii_whitespace())
            });
        let identity =
            identity.zip(token).map(
                |(id, token)| crate::daemon_credentials::AuthenticatedDaemon {
                    daemon_id: id.to_string(),
                    token_hash: crate::daemon_credentials::token_hash(token),
                },
            );
        let valid = match &identity {
            Some(identity) => match mode.store.daemon_credential_valid(identity).await {
                Ok(valid) => valid,
                Err(err) => {
                    tracing::error!(error = %err, "daemon authentication storage failure");
                    return error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "daemon authentication is unavailable",
                    );
                }
            },
            None => false,
        };
        if !valid {
            return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
                "unauthorized",
            )
                .into_response();
        }
        request
            .extensions_mut()
            .insert(identity.expect("validated daemon identity"));
    } else if path != "/v1/health" {
        let valid = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .filter(|token| !token.is_empty() && !token.bytes().any(|b| b.is_ascii_whitespace()))
            .is_some_and(|token| constant_time_token_eq(&mode.admin_token, token));
        if !valid {
            let mut response = error(StatusCode::UNAUTHORIZED, "invalid administrator credential");
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            if is_virtual_key_endpoint {
                response
                    .headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            }
            return response;
        }
    }
    let mut response = next.run(request).await;
    if is_virtual_key_endpoint {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

pub(crate) fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
        .into_response()
}

async fn not_found(uri: Uri) -> Response {
    error(
        StatusCode::NOT_FOUND,
        format!("no API route matches {}", uri.path()),
    )
}

async fn method_not_allowed() -> Response {
    error(
        StatusCode::METHOD_NOT_ALLOWED,
        "method not allowed for this path",
    )
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

async fn status(State(state): State<AppState>) -> Json<StatusResponse> {
    let (config_version, connected_daemons) = state.store.summary().await;
    Json(StatusResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds: now_ts() - state.started_at,
        connected_daemons: connected_daemons as i64,
        config_version,
    })
}

async fn get_config(State(state): State<AppState>) -> Response {
    let config = match state.access.store.config().await {
        Ok(config) => config,
        Err(err) => {
            tracing::error!(error = %err, "failed to read durable model configuration");
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "durable configuration is unavailable",
            );
        }
    };
    Json(crate::config_secrets::redact(config)).into_response()
}

async fn put_config(
    State(state): State<AppState>,
    payload: Result<Json<ModelConfigSet>, JsonRejection>,
) -> Response {
    let config = match payload {
        Ok(Json(config)) => config,
        Err(rejection) => return error(StatusCode::BAD_REQUEST, rejection.body_text()),
    };
    if let Err(message) = validate_config(&config) {
        return error(StatusCode::BAD_REQUEST, message);
    }

    let config = match state.access.store.replace_config(config).await {
        Ok(config) => config,
        Err(err) => {
            tracing::error!(error = %err, "failed to persist durable model configuration");
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "durable configuration is unavailable",
            );
        }
    };
    publish_config(&state, config).await
}

/// Make a stored configuration current here and on every connected daemon,
/// and answer with it redacted.
async fn publish_config(state: &AppState, config: ModelConfigSet) -> Response {
    state.store.set_config(config.clone()).await;

    let receivers = state
        .broadcast
        .send(crate::store::config_message(config.clone()))
        .unwrap_or(0);
    info!(
        version = %config.version,
        models = config.models.len(),
        receivers,
        "broadcasted configuration update"
    );

    Json(crate::config_secrets::redact(config)).into_response()
}

#[derive(Debug, Deserialize)]
struct VersionParams {
    version: Option<String>,
}

fn required_version(
    params: Result<Query<VersionParams>, QueryRejection>,
) -> Result<String, String> {
    let version = match params {
        Ok(Query(params)) => params.version.unwrap_or_default(),
        Err(rejection) => return Err(rejection.body_text()),
    };
    if version.trim().is_empty() {
        return Err("query parameter version is required and must not be empty".to_string());
    }
    Ok(version)
}

async fn get_model(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let config = match state.access.store.config().await {
        Ok(config) => config,
        Err(err) => {
            tracing::error!(error = %err, "failed to read durable model configuration");
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "durable configuration is unavailable",
            );
        }
    };
    let config = crate::config_secrets::redact(config);
    match config
        .models
        .into_iter()
        .find(|model| model.model_name == name)
    {
        Some(model) => Json(model).into_response(),
        None => error(StatusCode::NOT_FOUND, format!("model '{name}' not found")),
    }
}

async fn put_model(
    State(state): State<AppState>,
    Path(name): Path<String>,
    params: Result<Query<VersionParams>, QueryRejection>,
    payload: Result<Json<ModelConfig>, JsonRejection>,
) -> Response {
    let version = match required_version(params) {
        Ok(version) => version,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    let model = match payload {
        Ok(Json(model)) => model,
        Err(rejection) => return error(StatusCode::BAD_REQUEST, rejection.body_text()),
    };
    if model.model_name != name {
        return error(
            StatusCode::BAD_REQUEST,
            format!(
                "body model_name '{}' does not match the path's '{name}'",
                model.model_name
            ),
        );
    }
    change_model(&state, version, ModelChange::Upsert(model)).await
}

async fn delete_model(
    State(state): State<AppState>,
    Path(name): Path<String>,
    params: Result<Query<VersionParams>, QueryRejection>,
) -> Response {
    let version = match required_version(params) {
        Ok(version) => version,
        Err(message) => return error(StatusCode::BAD_REQUEST, message),
    };
    change_model(&state, version, ModelChange::Remove(name)).await
}

async fn change_model(state: &AppState, version: String, change: ModelChange) -> Response {
    let name = match &change {
        ModelChange::Upsert(model) => model.model_name.clone(),
        ModelChange::Remove(name) => name.clone(),
    };
    match state
        .access
        .store
        .change_model(version, change, validate_config)
        .await
    {
        Ok(ChangeOutcome::Stored { config, created }) => {
            info!(model = %name, version = %config.version, created, "changed one model");
            let mut response = publish_config(state, config).await;
            if created {
                *response.status_mut() = StatusCode::CREATED;
            }
            response
        }
        Ok(ChangeOutcome::NotFound) => {
            error(StatusCode::NOT_FOUND, format!("model '{name}' not found"))
        }
        Ok(ChangeOutcome::Invalid(message)) => error(StatusCode::BAD_REQUEST, message),
        Err(err) => {
            tracing::error!(error = %err, "failed to persist durable model configuration");
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "durable configuration is unavailable",
            )
        }
    }
}

#[derive(Debug, Deserialize)]
struct HistoryParams {
    limit: Option<i64>,
    cursor: Option<String>,
}

async fn list_config_history(
    State(state): State<AppState>,
    params: Result<Query<HistoryParams>, QueryRejection>,
) -> Response {
    let Query(params) = match params {
        Ok(params) => params,
        Err(rejection) => return error(StatusCode::BAD_REQUEST, rejection.body_text()),
    };
    let limit = params.limit.unwrap_or(DEFAULT_HISTORY_PAGE_SIZE);
    let max = crate::virtual_keys::CONFIG_HISTORY_LIMIT;
    if !(1..=max).contains(&limit) {
        return error(
            StatusCode::BAD_REQUEST,
            format!("limit must be between 1 and {max}"),
        );
    }
    let before = match params.cursor.as_deref().map(str::parse::<i64>) {
        None => None,
        Some(Ok(id)) if id > 0 => Some(id),
        Some(_) => return error(StatusCode::BAD_REQUEST, "cursor is invalid"),
    };

    let store = &state.access.store;
    // One extra row tells whether an older page exists.
    let (entries, current) =
        match tokio::try_join!(store.config_history(limit + 1, before), store.config()) {
            Ok(result) => result,
            Err(err) => {
                tracing::error!(error = %err, "failed to read model configuration history");
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "durable configuration is unavailable",
                );
            }
        };
    let more = entries.len() as i64 > limit;
    let entries = &entries[..entries.len().min(limit as usize)];
    let next_cursor = more
        .then(|| entries.last().map(|entry| entry.id.to_string()))
        .flatten();
    // History holds no keys; report the key a rollback would use, which is
    // the one stored now for the same model name.
    let current = crate::config_secrets::redact(current);
    let items = entries
        .iter()
        .map(|entry| {
            let mut config = entry.config.clone();
            for model in &mut config.models {
                model.api_key_configured = Some(current.models.iter().any(|stored| {
                    stored.model_name == model.model_name && stored.api_key_configured == Some(true)
                }));
            }
            ConfigHistoryEntry {
                id: entry.id,
                version: entry.version.clone(),
                created_at: entry.created_at,
                source: entry.source.clone(),
                rolled_back_from: entry.rolled_back_from,
                config,
            }
        })
        .collect();
    Json(ConfigHistoryPage { items, next_cursor }).into_response()
}

async fn rollback_config(
    State(state): State<AppState>,
    Path(id): Path<String>,
    payload: Result<Json<RollbackConfigRequest>, JsonRejection>,
) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return error(StatusCode::NOT_FOUND, "history entry not found");
    };
    let request = match payload {
        Ok(Json(request)) => request,
        Err(rejection) => return error(StatusCode::BAD_REQUEST, rejection.body_text()),
    };
    if request.version.trim().is_empty() {
        return error(StatusCode::BAD_REQUEST, "version must not be empty");
    }
    match state
        .access
        .store
        .rollback_config(id, request.version, validate_config)
        .await
    {
        Ok(RollbackOutcome::Restored(config)) => {
            info!(from = id, version = %config.version, "rolled back model configuration");
            publish_config(&state, config).await
        }
        Ok(RollbackOutcome::NotFound) => error(StatusCode::NOT_FOUND, "history entry not found"),
        Ok(RollbackOutcome::Invalid(message)) => error(
            StatusCode::BAD_REQUEST,
            format!("history entry {id} cannot be restored: {message}"),
        ),
        Err(err) => {
            tracing::error!(error = %err, "failed to roll back model configuration");
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "durable configuration is unavailable",
            )
        }
    }
}

fn validate_config(config: &ModelConfigSet) -> Result<(), String> {
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
        validate_api_base(model)
            .map_err(|message| format!("model '{}' {message}", model.model_name))?;
    }
    Ok(())
}

/// `api_base` is declared `format: uri`; the daemon appends `/chat/completions`
/// to it, so it must be an absolute http(s) URL without a fragment.
fn validate_api_base(model: &protocol::ModelConfig) -> Result<(), String> {
    let api_base = model.api_base.trim();
    if api_base.is_empty() {
        return Err("requires an api_base".to_string());
    }
    if api_base != model.api_base {
        return Err("has an api_base with surrounding whitespace".to_string());
    }
    let url = url::Url::parse(api_base)
        .map_err(|err| format!("has an api_base that is not an absolute URI ({err})"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "has an api_base with scheme '{}'; use http or https",
            url.scheme()
        ));
    }
    if url.fragment().is_some() {
        return Err(
            "has an api_base with a fragment, which the appended path would be behind".to_string(),
        );
    }
    Ok(())
}

async fn list_daemons(State(state): State<AppState>) -> Json<DaemonList> {
    Json(DaemonList {
        items: state.store.daemons().await,
    })
}

async fn get_daemon(State(state): State<AppState>, Path(connection_id): Path<String>) -> Response {
    match state.store.daemon(&connection_id).await {
        Some(daemon) => Json::<Daemon>(daemon).into_response(),
        None => error(
            StatusCode::NOT_FOUND,
            format!("daemon connection '{connection_id}' not found"),
        ),
    }
}

#[derive(Debug, Deserialize)]
struct UsageParams {
    limit: Option<usize>,
    cursor: Option<String>,
    daemon_id: Option<String>,
    model_name: Option<String>,
}

async fn list_usage_events(
    State(state): State<AppState>,
    params: Result<Query<UsageParams>, QueryRejection>,
) -> Response {
    let Query(params) = match params {
        Ok(params) => params,
        Err(rejection) => return error(StatusCode::BAD_REQUEST, rejection.body_text()),
    };

    let limit = params.limit.unwrap_or(DEFAULT_USAGE_PAGE_SIZE);
    if !(1..=USAGE_HISTORY_LIMIT).contains(&limit) {
        return error(
            StatusCode::BAD_REQUEST,
            format!("limit must be between 1 and {USAGE_HISTORY_LIMIT}"),
        );
    }
    let before = match params.cursor.as_deref().map(str::parse::<u64>) {
        None => None,
        Some(Ok(sequence)) => Some(sequence),
        Some(Err(_)) => return error(StatusCode::BAD_REQUEST, "cursor is invalid"),
    };

    let page = state
        .store
        .usage_events(&UsageQuery {
            limit,
            before,
            daemon_id: params.daemon_id,
            model_name: params.model_name,
        })
        .await;

    Json(UsageEventPage {
        items: page.items,
        next_cursor: page.next_before.map(|sequence| sequence.to_string()),
    })
    .into_response()
}

pub(crate) async fn publish_policy_hint(state: &AppState, mode: &crate::AccessControl) {
    match mode.store.snapshot(now_ts()).await {
        Ok(snapshot) => {
            let _ = state
                .broadcast
                .send(ControlMessage::VirtualKeyPolicySnapshot(snapshot));
        }
        Err(err) => {
            tracing::error!(error = %err, "failed to publish refreshed virtual-key policy snapshot");
        }
    }
}

/// Contract-first drift guard.
///
/// `contract/openapi.yaml` is authored before the handlers. These tests parse it
/// and exercise the real router, so a route or response shape that drifts from
/// the contract fails `cargo test`.
#[cfg(test)]
mod contract_tests {
    use std::{str::FromStr, sync::Arc};

    use axum::body::Body;
    use axum::http::Request;
    use protocol::{DaemonMessage, Registration, Telemetry};
    use serde_json::{json, Value};
    use tokio::sync::broadcast;
    use tower::ServiceExt;

    use super::*;
    use crate::store::{default_config, MemoryStore};

    const CONNECTION_ID: &str = "conn-test";

    fn contract() -> Value {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../contract/openapi.yaml");
        let raw = std::fs::read_to_string(path)
            .expect("contract/openapi.yaml should be readable from the crate root");
        serde_yaml::from_str(&raw).expect("contract/openapi.yaml should be valid YAML")
    }

    /// A router whose store holds one registered daemon with one usage event,
    /// so every collection and nested schema is exercised with real data.
    async fn seeded_app(cors: Vec<HeaderValue>) -> Router {
        seeded_app_with_broadcast(cors).await.0
    }

    /// As [`seeded_app`], also returning a receiver for what daemons are sent.
    async fn seeded_app_with_broadcast(
        cors: Vec<HeaderValue>,
    ) -> (Router, broadcast::Receiver<ControlMessage>) {
        let store = Arc::new(MemoryStore::new(default_config()));
        store.daemon_connected(CONNECTION_ID, 1).await;
        store
            .apply_daemon_message(
                CONNECTION_ID,
                DaemonMessage::Registration(Registration {
                    daemon_id: "daemon-1".to_string(),
                    cluster_name: "cluster-1".to_string(),
                    version: "0.1.0".to_string(),
                    capabilities: vec!["openai-compatible".to_string()],
                    hostname: "host-1".to_string(),
                }),
                2,
            )
            .await;
        for (request_id, model_name) in [("req-1", "m1"), ("req-2", "m2"), ("req-3", "m1")] {
            store
                .apply_daemon_message(
                    CONNECTION_ID,
                    DaemonMessage::Telemetry(Telemetry {
                        virtual_key_id: "a".repeat(32),
                        daemon_id: "daemon-1".to_string(),
                        model_name: model_name.to_string(),
                        prompt_tokens: 3,
                        completion_tokens: 4,
                        success: true,
                        request_id: request_id.to_string(),
                        latency_ms: 5,
                        status_code: None,
                        streamed: false,
                        usage_reported: false,
                    }),
                    3,
                )
                .await;
        }

        let (broadcast, receiver) = broadcast::channel(8);
        let key_store = match std::env::var("ONEFLOW_TEST_DATABASE_URL") {
            Ok(url) => crate::virtual_keys::VirtualKeyStore::connect(&url, &default_config())
                .await
                .unwrap(),
            Err(_) => crate::virtual_keys::VirtualKeyStore {
                pool: sqlx::postgres::PgPoolOptions::new()
                    .acquire_timeout(std::time::Duration::from_millis(100))
                    .connect_lazy("postgres://unused@127.0.0.1:1/unused")
                    .unwrap(),
            },
        };
        let app = create_app(
            AppState {
                store,
                broadcast,
                started_at: now_ts(),
                access: Arc::new(crate::AccessControl {
                    store: Arc::new(key_store),
                    admin_token: Arc::new("test-admin-token".into()),
                    control_plane_url: "wss://control.example/control".into(),
                }),
            },
            cors,
        );
        (app, receiver)
    }

    async fn app() -> Router {
        seeded_app(Vec::new()).await
    }

    fn request(method: &str, path: &str, body: Option<&str>) -> Request<Body> {
        let builder = Request::builder()
            .method(Method::from_str(&method.to_ascii_uppercase()).expect("valid HTTP method"))
            .uri(path)
            .header(header::AUTHORIZATION, "Bearer test-admin-token")
            .header("content-type", "application/json");
        match body {
            Some(body) => builder.body(Body::from(body.to_string())),
            None => builder.body(Body::empty()),
        }
        .expect("request should build")
    }

    async fn call(
        app: Router,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> (StatusCode, Value) {
        let response = app
            .oneshot(request(method, path, body))
            .await
            .expect("router should respond");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    fn concrete_path(path: &str) -> String {
        path.replace("{connection_id}", CONNECTION_ID)
            .replace("{daemon_id}", "missing-test-daemon")
            .replace("{id}", "0")
            .replace(
                "{model_name}",
                &default_config().models[0].model_name.clone(),
            )
    }

    fn schema_ref_name(schema: &Value) -> Option<&str> {
        schema["$ref"]
            .as_str()
            .and_then(|reference| reference.strip_prefix("#/components/schemas/"))
    }

    /// Recursively checks that `value` carries every field the contract marks
    /// as required, following `$ref`s into nested objects and array items.
    fn assert_matches_schema(contract: &Value, schema_name: &str, value: &Value, at: &str) {
        let schema = &contract["components"]["schemas"][schema_name];
        assert!(schema.is_object(), "contract has no schema `{schema_name}`");

        for field in schema["required"].as_array().into_iter().flatten() {
            let field = field.as_str().expect("required entries are strings");
            assert!(
                value.get(field).is_some(),
                "{at}: `{schema_name}.{field}` is required by the contract but missing"
            );
        }

        let Some(properties) = schema["properties"].as_object() else {
            return;
        };
        for (name, property) in properties {
            let Some(child) = value.get(name) else {
                continue;
            };
            if let Some(nested) = schema_ref_name(property) {
                assert_matches_schema(contract, nested, child, &format!("{at}.{name}"));
            }
            if let Some(nested) = schema_ref_name(&property["items"]) {
                for (index, item) in child.as_array().into_iter().flatten().enumerate() {
                    assert_matches_schema(contract, nested, item, &format!("{at}.{name}[{index}]"));
                }
            }
        }
    }

    #[tokio::test]
    async fn every_contract_operation_is_routed() {
        let contract = contract();
        let paths = contract["paths"]
            .as_object()
            .expect("contract should declare paths");

        for (path, item) in paths {
            for method in item.as_object().expect("path item is an object").keys() {
                if !matches!(method.as_str(), "get" | "post" | "put" | "patch" | "delete") {
                    continue;
                }
                let (status, body) = call(app().await, method, &concrete_path(path), None).await;
                if status == StatusCode::NOT_FOUND {
                    assert!(
                        !body["error"]
                            .as_str()
                            .unwrap()
                            .starts_with("no API route matches"),
                        "contract declares {method} {path} but the router does not serve it"
                    );
                }
                assert_ne!(
                    status,
                    StatusCode::METHOD_NOT_ALLOWED,
                    "contract declares {method} {path} but the router rejects that method"
                );
            }
        }
    }

    #[tokio::test]
    async fn every_get_response_matches_its_contract_schema() {
        if std::env::var_os("ONEFLOW_TEST_DATABASE_URL").is_none() {
            eprintln!("Skipping database-backed response contracts: set ONEFLOW_TEST_DATABASE_URL");
            return;
        }
        let _config_lock = crate::SHARED_CONFIG_TEST_LOCK.lock().await;
        let contract = contract();
        let app = app().await;
        let (_, config) = call(app.clone(), "get", "/v1/config", None).await;
        let (status, created) = call(
            app.clone(),
            "post",
            "/key/generate",
            Some(&json!({"models": [config["models"][0]["model_name"]]}).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        for (path, item) in contract["paths"].as_object().unwrap() {
            let Some(operation) = item.get("get") else {
                continue;
            };
            let schema = &operation["responses"]["200"]["content"]["application/json"]["schema"];
            let schema_name = schema_ref_name(schema)
                .unwrap_or_else(|| panic!("GET {path} 200 response should reference a schema"));

            let path = if path == "/key/info" {
                format!("/key/info?key={}", created["token"].as_str().unwrap())
            } else if path == "/v1/models/{model_name}" {
                // Other tests change the shared configuration; use what is current.
                format!(
                    "/v1/models/{}",
                    config["models"][0]["model_name"].as_str().unwrap()
                )
            } else {
                concrete_path(path)
            };
            let (status, body) = call(app.clone(), "get", &path, None).await;
            assert_eq!(status, StatusCode::OK, "GET {path}");
            assert_matches_schema(&contract, schema_name, &body, &format!("GET {path}"));
        }
        assert_eq!(
            call(
                app,
                "post",
                "/key/delete",
                Some(&json!({"keys":[created["token"]]}).to_string())
            )
            .await
            .0,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn config_put_round_trips_and_is_reported_in_status() {
        if std::env::var_os("ONEFLOW_TEST_DATABASE_URL").is_none() {
            eprintln!("Skipping durable config test: set ONEFLOW_TEST_DATABASE_URL");
            return;
        }
        let _config_lock = crate::SHARED_CONFIG_TEST_LOCK.lock().await;
        let app = app().await;
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

        let (status, _) = call(app.clone(), "put", "/v1/config", Some(&payload)).await;
        assert_eq!(status, StatusCode::OK);

        let (_, config) = call(app.clone(), "get", "/v1/config", None).await;
        assert_eq!(config["version"], "contract-test");
        assert_eq!(
            config["models"][0]["parameters"]["upstream_model"],
            "test-upstream"
        );
        let (_, status) = call(app, "get", "/v1/status", None).await;
        assert_eq!(status["config_version"], "contract-test");
        assert_eq!(status["connected_daemons"], 1);
    }

    /// The literal provider key is never returned by the management API, an
    /// operator can round-trip a GET body without losing it, and daemons still
    /// receive it on the control channel.
    #[tokio::test]
    async fn provider_api_key_is_write_only_but_reaches_daemons() {
        if std::env::var_os("ONEFLOW_TEST_DATABASE_URL").is_none() {
            eprintln!("Skipping provider key test: set ONEFLOW_TEST_DATABASE_URL");
            return;
        }
        let _config_lock = crate::SHARED_CONFIG_TEST_LOCK.lock().await;
        const SECRET: &str = "sk-provider-write-only-test";
        let (app, mut daemons) = seeded_app_with_broadcast(Vec::new()).await;
        let model = |name: &str, parameters: Value| {
            json!({
                "model_name": name,
                "provider": "custom_openai",
                "api_base": "https://example.test/v1",
                "parameters": parameters,
            })
        };
        let broadcast_key = |message: ControlMessage, name: &str| match message {
            ControlMessage::ConfigUpdate { models, .. } => models
                .into_iter()
                .find(|model| model.model_name == name)
                .and_then(|model| model.parameters.get("api_key").cloned()),
            other => panic!("expected ConfigUpdate, got {other:?}"),
        };

        // Write a key, plus a client-sent `api_key_configured` that must be ignored.
        let mut keyed = model("keyed", json!({ "api_key": SECRET, "upstream_model": "u" }));
        keyed["api_key_configured"] = json!(false);
        let body = json!({
            "version": "secret-v1",
            "models": [keyed, model("plain", json!({}))],
        });
        let (status, put) = call(app.clone(), "put", "/v1/config", Some(&body.to_string())).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!put.to_string().contains(SECRET), "PUT echoed the key");
        assert_eq!(put["models"][0]["api_key_configured"], true);
        assert_eq!(put["models"][1]["api_key_configured"], false);
        assert_eq!(
            broadcast_key(daemons.recv().await.unwrap(), "keyed").as_deref(),
            Some(SECRET)
        );

        // GET never returns the key.
        let (_, got) = call(app.clone(), "get", "/v1/config", None).await;
        assert!(!got.to_string().contains(SECRET), "GET returned the key");
        assert!(got["models"][0]["parameters"].get("api_key").is_none());
        assert_eq!(got["models"][0]["parameters"]["upstream_model"], "u");
        assert_eq!(got["models"][0]["api_key_configured"], true);

        // Sending the GET body back keeps the key for daemons.
        let mut round_trip = got.clone();
        round_trip["version"] = json!("secret-v2");
        let (status, _) = call(
            app.clone(),
            "put",
            "/v1/config",
            Some(&round_trip.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            broadcast_key(daemons.recv().await.unwrap(), "keyed").as_deref(),
            Some(SECRET)
        );

        // An empty key removes it.
        let mut removal = got;
        removal["version"] = json!("secret-v3");
        removal["models"][0]["parameters"]["api_key"] = json!("");
        let (status, removed) =
            call(app.clone(), "put", "/v1/config", Some(&removal.to_string())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(removed["models"][0]["api_key_configured"], false);
        assert_eq!(broadcast_key(daemons.recv().await.unwrap(), "keyed"), None);
    }

    /// Every write lands in history without its key; a rollback restores the
    /// models under a new version, reuses the key stored now, reaches daemons
    /// and is itself recorded.
    #[tokio::test]
    async fn config_history_records_writes_and_rolls_back() {
        if std::env::var_os("ONEFLOW_TEST_DATABASE_URL").is_none() {
            eprintln!("Skipping config history test: set ONEFLOW_TEST_DATABASE_URL");
            return;
        }
        let _config_lock = crate::SHARED_CONFIG_TEST_LOCK.lock().await;
        const SECRET: &str = "sk-history-secret";
        let (app, mut daemons) = seeded_app_with_broadcast(Vec::new()).await;
        let put = |version: &str, models: Value| {
            let app = app.clone();
            let body = json!({ "version": version, "models": models }).to_string();
            async move { call(app, "put", "/v1/config", Some(&body)).await }
        };
        let model = |name: &str, parameters: Value| {
            json!({
                "model_name": name,
                "provider": "custom_openai",
                "api_base": "https://example.test/v1",
                "parameters": parameters,
            })
        };
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let v1 = format!("history-a-{tag}");
        let v2 = format!("history-b-{tag}");

        let (status, _) = put(
            &v1,
            json!([
                model("keyed", json!({ "api_key": SECRET })),
                model("old", json!({}))
            ]),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = put(
            &v2,
            json!([model("keyed", json!({ "upstream_model": "u2" }))]),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        while daemons.try_recv().is_ok() {}

        let (status, page) = call(app.clone(), "get", "/v1/config/history?limit=2", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !page.to_string().contains(SECRET),
            "history returned the key"
        );
        let items = page["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["version"], v2.as_str());
        assert_eq!(items[0]["source"], "put");
        assert_eq!(items[1]["version"], v1.as_str());
        assert_eq!(items[1]["config"]["models"][0]["api_key_configured"], true);
        assert_eq!(items[1]["config"]["models"][1]["api_key_configured"], false);
        let v1_id = items[1]["id"].as_i64().unwrap();
        let cursor = page["next_cursor"].as_str().expect("older entries remain");
        let (_, older) = call(
            app.clone(),
            "get",
            &format!("/v1/config/history?limit=1&cursor={cursor}"),
            None,
        )
        .await;
        assert!(older["items"][0]["id"].as_i64().unwrap() < v1_id);

        // Nothing stored holds the key, not even the history table itself.
        let store = crate::virtual_keys::VirtualKeyStore::connect(
            &std::env::var("ONEFLOW_TEST_DATABASE_URL").unwrap(),
            &default_config(),
        )
        .await
        .unwrap();
        let leaked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM model_config_history WHERE config::text LIKE $1)",
        )
        .bind(format!("%{SECRET}%"))
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert!(!leaked, "history stored the key");

        let v3 = format!("history-c-{tag}");
        let (status, restored) = call(
            app.clone(),
            "post",
            &format!("/v1/config/history/{v1_id}/rollback"),
            Some(&json!({ "version": v3 }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(restored["version"], v3.as_str());
        assert_eq!(restored["models"].as_array().unwrap().len(), 2);
        assert_eq!(restored["models"][0]["api_key_configured"], true);
        assert!(!restored.to_string().contains(SECRET));
        match daemons.recv().await.unwrap() {
            ControlMessage::ConfigUpdate {
                models, version, ..
            } => {
                assert_eq!(version, v3);
                assert_eq!(
                    models[0].parameters.get("api_key").map(String::as_str),
                    Some(SECRET)
                );
                assert_eq!(models[1].model_name, "old");
            }
            other => panic!("expected ConfigUpdate, got {other:?}"),
        }
        let (_, current) = call(app.clone(), "get", "/v1/config", None).await;
        assert_eq!(current["version"], v3.as_str());
        let (_, page) = call(app.clone(), "get", "/v1/config/history?limit=1", None).await;
        assert_eq!(page["items"][0]["source"], "rollback");
        assert_eq!(page["items"][0]["rolled_back_from"], v1_id);

        // A row stored before api_base was validated cannot come back.
        let stale: i64 = sqlx::query_scalar(
            "INSERT INTO model_config_history (version, config, created_at, source) \
             VALUES ('stale', $1, 1, 'put') RETURNING id",
        )
        .bind(json!({
            "version": "stale",
            "models": [{
                "model_name": "legacy",
                "provider": "custom_openai",
                "api_base": "legacy.example/v1",
                "parameters": {},
            }],
        }))
        .fetch_one(&store.pool)
        .await
        .unwrap();
        let (status, response) = call(
            app.clone(),
            "post",
            &format!("/v1/config/history/{stale}/rollback"),
            Some(&json!({ "version": "stale-restored" }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            response["error"]
                .as_str()
                .unwrap()
                .contains("not an absolute URI"),
            "unexpected error: {response}"
        );
        let (_, current) = call(app.clone(), "get", "/v1/config", None).await;
        assert_eq!(
            current["version"],
            v3.as_str(),
            "a rejected rollback changed the config"
        );

        for (path, body, expected) in [
            (
                "/v1/config/history/0/rollback",
                json!({ "version": "x" }),
                StatusCode::NOT_FOUND,
            ),
            (
                "/v1/config/history/abc/rollback",
                json!({ "version": "x" }),
                StatusCode::NOT_FOUND,
            ),
            (
                &*format!("/v1/config/history/{v1_id}/rollback"),
                json!({ "version": " " }),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let (status, response) = call(app.clone(), "post", path, Some(&body.to_string())).await;
            assert_eq!(status, expected, "{path} {body}");
            assert!(response["error"].is_string());
        }
    }

    #[tokio::test]
    async fn invalid_history_queries_are_json_400() {
        for query in ["limit=0", "limit=101", "cursor=0", "cursor=abc"] {
            let (status, body) = call(
                app().await,
                "get",
                &format!("/v1/config/history?{query}"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "query: {query}");
            assert!(body["error"].is_string(), "query: {query}");
        }
    }

    #[tokio::test]
    async fn invalid_config_is_a_json_400() {
        for body in [
            json!({ "version": "", "models": [] }).to_string(),
            "not json".to_string(),
            json!({ "version": "v" }).to_string(),
        ] {
            let (status, response) = call(app().await, "put", "/v1/config", Some(&body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
            assert!(response["error"].is_string(), "body: {body}");
        }
    }

    #[tokio::test]
    async fn single_model_crud_changes_only_that_model() {
        if std::env::var_os("ONEFLOW_TEST_DATABASE_URL").is_none() {
            eprintln!("Skipping model CRUD test: set ONEFLOW_TEST_DATABASE_URL");
            return;
        }
        let _config_lock = crate::SHARED_CONFIG_TEST_LOCK.lock().await;
        const SECRET: &str = "sk-model-crud-secret";
        let (app, mut daemons) = seeded_app_with_broadcast(Vec::new()).await;
        let model = |name: &str, parameters: Value| {
            json!({
                "model_name": name,
                "provider": "custom_openai",
                "api_base": "https://example.test/v1",
                "parameters": parameters,
            })
        };
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let body = json!({
            "version": format!("crud-a-{tag}"),
            "models": [model("keyed", json!({ "api_key": SECRET })), model("other", json!({}))],
        });
        let (status, _) = call(app.clone(), "put", "/v1/config", Some(&body.to_string())).await;
        assert_eq!(status, StatusCode::OK);
        while daemons.try_recv().is_ok() {}

        // Read one, redacted.
        let (status, one) = call(app.clone(), "get", "/v1/models/keyed", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(one["api_key_configured"], true);
        assert!(!one.to_string().contains(SECRET));
        let (status, _) = call(app.clone(), "get", "/v1/models/nope", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Replace keeps the stored key and the other model; daemons get it.
        let v2 = format!("crud-b-{tag}");
        let (status, stored) = call(
            app.clone(),
            "put",
            &format!("/v1/models/keyed?version={v2}"),
            Some(&model("keyed", json!({ "upstream_model": "u2" })).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(stored["version"], v2.as_str());
        assert_eq!(stored["models"].as_array().unwrap().len(), 2);
        assert_eq!(stored["models"][0]["api_key_configured"], true);
        assert_eq!(stored["models"][0]["parameters"]["upstream_model"], "u2");
        match daemons.recv().await.unwrap() {
            ControlMessage::ConfigUpdate {
                models, version, ..
            } => {
                assert_eq!(version, v2);
                assert_eq!(
                    models[0].parameters.get("api_key").map(String::as_str),
                    Some(SECRET)
                );
                assert_eq!(models[1].model_name, "other");
            }
            other => panic!("expected ConfigUpdate, got {other:?}"),
        }

        // Adding is a 201 and appends.
        let (status, stored) = call(
            app.clone(),
            "put",
            &format!("/v1/models/fresh?version=crud-c-{tag}"),
            Some(&model("fresh", json!({})).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(stored["models"][2]["model_name"], "fresh");

        // Bad requests store nothing.
        for (method, path, body, expected) in [
            (
                "put",
                "/v1/models/fresh".to_string(),
                Some(model("fresh", json!({}))),
                "version",
            ),
            (
                "put",
                format!("/v1/models/fresh?version=x-{tag}"),
                Some(model("other", json!({}))),
                "does not match",
            ),
            (
                "put",
                format!("/v1/models/fresh?version=x-{tag}"),
                Some(
                    json!({ "model_name": "fresh", "provider": "p", "api_base": "fresh.example", "parameters": {} }),
                ),
                "not an absolute URI",
            ),
            (
                "delete",
                "/v1/models/fresh?version=".to_string(),
                None,
                "version",
            ),
        ] {
            let body = body.map(|body| body.to_string());
            let (status, response) = call(app.clone(), method, &path, body.as_deref()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {path}");
            assert!(
                response["error"].as_str().unwrap().contains(expected),
                "{method} {path}: {response}"
            );
        }
        let (_, current) = call(app.clone(), "get", "/v1/config", None).await;
        assert_eq!(current["version"], format!("crud-c-{tag}").as_str());

        // Delete removes the model and its key; the last one cannot go.
        let (status, stored) = call(
            app.clone(),
            "delete",
            &format!("/v1/models/keyed?version=crud-d-{tag}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let names: Vec<&str> = stored["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["model_name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["other", "fresh"]);
        let (status, _) = call(
            app.clone(),
            "delete",
            &format!("/v1/models/keyed?version=crud-e-{tag}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(
            app.clone(),
            "delete",
            &format!("/v1/models/fresh?version=crud-f-{tag}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, response) = call(
            app.clone(),
            "delete",
            &format!("/v1/models/other?version=crud-g-{tag}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        let (_, page) = call(app.clone(), "get", "/v1/config/history?limit=1", None).await;
        assert_eq!(
            page["items"][0]["version"],
            format!("crud-f-{tag}").as_str()
        );
    }

    /// `api_base` must be an absolute http(s) URI the daemon can append to.
    #[tokio::test]
    async fn api_base_must_be_an_absolute_http_uri() {
        let body = |api_base: &str| {
            json!({
                "version": "uri-test",
                "models": [{
                    "model_name": "m",
                    "provider": "custom_openai",
                    "api_base": api_base,
                    "parameters": {},
                }],
            })
            .to_string()
        };
        for (api_base, expected) in [
            ("", "requires an api_base"),
            ("   ", "requires an api_base"),
            (" https://example.test/v1", "surrounding whitespace"),
            ("api.example.test/v1", "not an absolute URI"),
            ("/v1", "not an absolute URI"),
            ("https://", "not an absolute URI"),
            ("ftp://example.test/v1", "use http or https"),
            ("file:///srv/models", "use http or https"),
            ("mailto:ops@example.test", "use http or https"),
            ("https://example.test/v1#frag", "fragment"),
        ] {
            let (status, response) =
                call(app().await, "put", "/v1/config", Some(&body(api_base))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "api_base: {api_base:?}");
            let message = response["error"].as_str().unwrap();
            assert!(
                message.contains(expected) && message.contains("'m'"),
                "api_base {api_base:?}: unexpected message {message:?}"
            );
        }
    }

    #[tokio::test]
    async fn daemon_lookup_returns_the_record_or_a_json_404() {
        let (status, daemon) = call(app().await, "get", "/v1/daemons/conn-test", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(daemon["daemon_id"], "daemon-1");
        assert_eq!(daemon["total_requests"], 3);

        let (status, body) = call(app().await, "get", "/v1/daemons/missing", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"].as_str().unwrap().contains("missing"));
    }

    #[tokio::test]
    async fn usage_events_page_newest_first_with_filters() {
        let app = app().await;

        let (_, first) = call(app.clone(), "get", "/v1/usage/events?limit=2", None).await;
        assert_eq!(first["items"][0]["request_id"], "req-3");
        assert_eq!(first["items"][1]["request_id"], "req-2");
        let cursor = first["next_cursor"].as_str().expect("more events remain");

        let (_, second) = call(
            app.clone(),
            "get",
            &format!("/v1/usage/events?limit=2&cursor={cursor}"),
            None,
        )
        .await;
        assert_eq!(second["items"][0]["request_id"], "req-1");
        assert!(second["next_cursor"].is_null());

        let (_, filtered) = call(app, "get", "/v1/usage/events?model_name=m2", None).await;
        assert_eq!(filtered["items"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn invalid_usage_queries_are_json_400() {
        for query in ["limit=0", "limit=201", "limit=abc", "cursor=not-a-cursor"] {
            let (status, body) = call(
                app().await,
                "get",
                &format!("/v1/usage/events?{query}"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "query: {query}");
            assert!(body["error"].is_string(), "query: {query}");
        }
    }

    #[tokio::test]
    async fn all_management_and_control_routes_require_authentication() {
        let app = app().await;
        let contract = contract();
        for (path, item) in contract["paths"].as_object().unwrap() {
            if path == "/v1/health" {
                continue;
            }
            for method in item
                .as_object()
                .unwrap()
                .keys()
                .filter(|method| matches!(method.as_str(), "get" | "post" | "put"))
            {
                let mut request = request(method, &concrete_path(path), None);
                request.headers_mut().remove(header::AUTHORIZATION);
                let response = app.clone().oneshot(request).await.unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::UNAUTHORIZED,
                    "{method} {path}"
                );
                assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
            }
        }
        for supplied in [None, Some("Bearer invalid")] {
            let mut request = request("GET", "/control", None);
            request.headers_mut().remove(header::AUTHORIZATION);
            if let Some(value) = supplied {
                request
                    .headers_mut()
                    .insert(header::AUTHORIZATION, HeaderValue::from_static(value));
            }
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::UNAUTHORIZED
            );
        }
        let mut request = request("GET", "/v1/health", None);
        request.headers_mut().remove(header::AUTHORIZATION);
        assert_eq!(app.oneshot(request).await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn credential_routes_fail_explicitly_when_database_is_unavailable() {
        let (broadcast, _) = broadcast::channel(8);
        let app = create_app(
            AppState {
                store: Arc::new(MemoryStore::new(default_config())),
                broadcast,
                started_at: now_ts(),
                access: Arc::new(crate::AccessControl {
                    admin_token: Arc::new("test-admin-token".into()),
                    control_plane_url: "wss://control.example/control".into(),
                    store: Arc::new(crate::virtual_keys::VirtualKeyStore {
                        pool: sqlx::postgres::PgPoolOptions::new()
                            .acquire_timeout(std::time::Duration::from_millis(100))
                            .connect_lazy("postgres://unused@127.0.0.1:1/unused")
                            .unwrap(),
                    }),
                }),
            },
            vec![],
        );
        for (method, path, body) in [
            ("get", "/v1/config", None),
            ("put", "/v1/config", Some("{\"version\":\"test\",\"models\":[{\"model_name\":\"test\",\"provider\":\"test\",\"api_base\":\"https://example.invalid\",\"parameters\":{}}]}")),
            ("get", "/key/list", None),
            ("post", "/key/generate", Some("{\"models\":[\"test\"]}")),
            ("get", "/key/info?key=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", None),
            ("post", "/key/delete", Some("{\"keys\":[\"unknown\"]}")),
            ("post", "/v1/daemons", Some("{\"daemon_id\":\"test\"}")),
            ("get", "/v1/daemons/credentials", None),
            ("post", "/v1/daemons/test/rotate", None),
            ("post", "/v1/daemons/test/revoke", None),
        ] {
            let (status, response) = call(app.clone(), method, path, body).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{method} {path}");
            assert!(response["error"].is_string());
        }
    }

    /// The control plane is API-only: no dashboard, and every miss is JSON.
    #[tokio::test]
    async fn unknown_paths_and_methods_are_json_errors() {
        for path in ["/", "/index.html", "/api/status", "/v1/not-in-the-contract"] {
            let (status, body) = call(app().await, "get", path, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "path: {path}");
            assert!(body["error"].is_string(), "path: {path}");
        }

        let (status, body) = call(app().await, "delete", "/v1/config", None).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert!(body["error"].is_string());
    }

    #[tokio::test]
    async fn cors_is_only_granted_to_configured_origins() {
        let preflight = |origin: &str| {
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/v1/config")
                .header(header::ORIGIN, origin)
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "PUT")
                .body(Body::empty())
                .unwrap()
        };
        let allowed = HeaderValue::from_static("http://127.0.0.1:5173");

        let response = seeded_app(vec![allowed.clone()])
            .await
            .oneshot(preflight("http://127.0.0.1:5173"))
            .await
            .unwrap();
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&allowed)
        );

        let response = seeded_app(vec![allowed])
            .await
            .oneshot(preflight("http://evil.example"))
            .await
            .unwrap();
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());

        let response = app()
            .await
            .oneshot(preflight("http://127.0.0.1:5173"))
            .await
            .unwrap();
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
    }
}
