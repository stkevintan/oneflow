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
    ControlMessage, Daemon, DaemonList, ErrorResponse, HealthResponse, ModelConfigSet,
    StatusResponse, UsageEventPage,
};
use serde::Deserialize;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::info;

use crate::{
    constant_time_token_eq, control, now_ts,
    store::{UsageQuery, USAGE_HISTORY_LIMIT},
    AppState,
};

const DEFAULT_USAGE_PAGE_SIZE: usize = 50;

pub fn create_app(state: AppState, cors_allowed_origins: Vec<HeaderValue>) -> Router {
    let router = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/status", get(status))
        .route("/v1/config", get(get_config).put(put_config))
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
    if let Some(mode) = &state.virtual_key_mode {
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
            let identity = identity.zip(token).map(|(id, token)| {
                crate::daemon_credentials::AuthenticatedDaemon {
                    daemon_id: id.to_string(),
                    token_hash: crate::daemon_credentials::token_hash(token),
                }
            });
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
                .filter(|token| {
                    !token.is_empty() && !token.bytes().any(|b| b.is_ascii_whitespace())
                })
                .is_some_and(|token| constant_time_token_eq(&mode.admin_token, token));
            if !valid {
                let mut response =
                    error(StatusCode::UNAUTHORIZED, "invalid administrator credential");
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
    let config = match &state.virtual_key_mode {
        Some(mode) => match mode.store.config().await {
            Ok(config) => config,
            Err(err) => {
                tracing::error!(error = %err, "failed to read durable model configuration");
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "durable configuration is unavailable",
                );
            }
        },
        None => state.store.config().await,
    };
    Json(config).into_response()
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

    if let Some(mode) = &state.virtual_key_mode {
        if let Err(err) = mode.store.set_config(&config).await {
            tracing::error!(error = %err, "failed to persist durable model configuration");
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "durable configuration is unavailable",
            );
        }
    }
    state.store.set_config(config.clone()).await;

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
        if model.api_base.trim().is_empty() {
            return Err(format!("model '{}' requires an api_base", model.model_name));
        }
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

pub(crate) async fn publish_policy_hint(state: &AppState, mode: &crate::VirtualKeyMode) {
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
                        virtual_key_id: None,
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

        let (broadcast, _) = broadcast::channel(8);
        create_app(
            AppState {
                store,
                broadcast,
                started_at: now_ts(),
                virtual_key_mode: None,
            },
            cors,
        )
    }

    async fn app() -> Router {
        seeded_app(Vec::new()).await
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
                let (status, _) = call(app().await, method, &concrete_path(path), None).await;
                assert_ne!(
                    status,
                    StatusCode::NOT_FOUND,
                    "contract declares {method} {path} but the router does not serve it"
                );
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
        let contract = contract();
        for (path, item) in contract["paths"].as_object().unwrap() {
            let Some(operation) = item.get("get") else {
                continue;
            };
            if path.starts_with("/key/") || path == "/v1/daemons/credentials" {
                let (status, body) = call(app().await, "get", path, None).await;
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "GET {path}");
                assert!(body["error"].is_string());
                continue;
            }
            let schema = &operation["responses"]["200"]["content"]["application/json"]["schema"];
            let schema_name = schema_ref_name(schema)
                .unwrap_or_else(|| panic!("GET {path} 200 response should reference a schema"));

            let (status, body) = call(app().await, "get", &concrete_path(path), None).await;
            assert_eq!(status, StatusCode::OK, "GET {path}");
            assert_matches_schema(&contract, schema_name, &body, &format!("GET {path}"));
        }
    }

    #[tokio::test]
    async fn config_put_round_trips_and_is_reported_in_status() {
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
    async fn credential_routes_report_unavailable_without_opt_in_storage() {
        for (method, path, body) in [
            ("get", "/key/list", None),
            ("post", "/key/generate", Some("{}")),
            ("get", "/key/info?key=unknown", None),
            ("post", "/key/delete", Some("{\"keys\":[]}")),
            ("post", "/v1/daemons", Some("{\"daemon_id\":\"test\"}")),
            ("get", "/v1/daemons/credentials", None),
            ("post", "/v1/daemons/test/rotate", None),
            ("post", "/v1/daemons/test/revoke", None),
        ] {
            let (status, response) = call(app().await, method, path, body).await;
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
