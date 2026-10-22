//! HTTP surface: the OpenAI-compatible `/v1/*` routes plus `/healthz`.

use std::sync::atomic::Ordering;
use std::time::Instant;

use axum::{
    body::Body,
    extract::{rejection::JsonRejection, Path, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use futures_util::StreamExt;
use protocol::ModelConfig;
use serde_json::{json, Value};
use tracing::warn;
use uuid::Uuid;

use crate::auth::{AuthFailure, AuthMode, Principal};
use crate::upstream::{
    self, apply_model_parameters, chat_completions_endpoint, configured_api_key,
    enable_stream_usage, RequestContext, TelemetryGuard, Usage,
};
use crate::{now_ts, ActiveRequestGuard, AppState};

pub(crate) fn create_app(state: AppState) -> Router {
    let protected = Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/models/:model_name", get(get_model))
        .route("/v1/chat/completions", post(chat_completions))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_api_key,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .merge(protected)
        .with_state(state)
}

/// Rejects unauthenticated `/v1/*` calls when `DAEMON_API_KEYS` is configured.
/// This is deliberately local and temporary: Phase 2 replaces it with
/// control-plane-issued virtual keys.
async fn require_api_key(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    if state.settings.auth_mode == AuthMode::VirtualKeys {
        let token = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        let principal = match token {
            Some(token) => state.policies.read().await.authenticate(
                token,
                now_ts(),
                tokio::time::Instant::now(),
            ),
            None => Err(AuthFailure::InvalidKey),
        };
        match principal {
            Ok(principal) => {
                request.extensions_mut().insert(principal);
                return next.run(request).await;
            }
            Err(failure) => {
                let (status, code, message) = match failure {
                    AuthFailure::InvalidKey => (
                        StatusCode::UNAUTHORIZED,
                        "invalid_api_key",
                        "missing or invalid API key",
                    ),
                    AuthFailure::Unavailable => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "auth_policy_unavailable",
                        "authorization policy is unavailable",
                    ),
                };
                warn!(%status, code, "inference authentication rejected");
                return auth_error(status, code, message, None);
            }
        }
    }
    if state.settings.api_keys.is_empty() {
        return next.run(request).await;
    }

    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);

    match provided {
        Some(key)
            if state
                .settings
                .api_keys
                .iter()
                .any(|expected| constant_time_eq(expected, key)) =>
        {
            next.run(request).await
        }
        _ => {
            let mut response = openai_error(StatusCode::UNAUTHORIZED, "missing or invalid API key");
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            response
        }
    }
}

fn constant_time_eq(expected: &str, provided: &str) -> bool {
    use subtle::ConstantTimeEq;
    bool::from(expected.as_bytes().ct_eq(provided.as_bytes()))
}

async fn healthz(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "daemon_id": state.settings.daemon_id,
        "uptime_seconds": now_ts() - state.started_at,
        "models": state.model_count().await,
        "active_requests": state.active_requests.load(Ordering::Relaxed),
        "dropped_telemetry": state.dropped_telemetry.load(Ordering::Relaxed),
    }))
}

async fn list_models(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
) -> Json<Value> {
    let created = state.models_updated_at.load(Ordering::Relaxed);
    let mut data: Vec<Value> = state
        .models
        .read()
        .await
        .values()
        .filter(|model| {
            principal
                .as_ref()
                .is_none_or(|Extension(key)| key.allows(&model.model_name))
        })
        .map(|model| model_json(model, created))
        .collect();
    data.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));

    Json(json!({ "object": "list", "data": data }))
}

async fn get_model(
    State(state): State<AppState>,
    Path(model_name): Path<String>,
    principal: Option<Extension<Principal>>,
) -> Response {
    if principal
        .as_ref()
        .is_some_and(|Extension(key)| !key.allows(&model_name))
    {
        return model_forbidden();
    }
    match state.model(&model_name).await {
        Some(model) => {
            let created = state.models_updated_at.load(Ordering::Relaxed);
            Json(model_json(&model, created)).into_response()
        }
        None => auth_error(
            StatusCode::NOT_FOUND,
            "model_not_found",
            format!("model '{model_name}' is not configured"),
            Some("model"),
        ),
    }
}

fn model_json(model: &ModelConfig, created: i64) -> Value {
    json!({
        "id": model.model_name,
        "object": "model",
        "created": created,
        "owned_by": model.provider,
    })
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    principal: Option<Extension<Principal>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    let mut payload = match payload {
        Ok(Json(value)) => value,
        Err(_) => {
            return auth_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "invalid JSON body",
                None,
            )
        }
    };
    let requested_model = match payload.get("model").and_then(Value::as_str) {
        Some(model) if !model.trim().is_empty() => model.to_string(),
        _ => {
            return auth_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "the model field is required",
                Some("model"),
            )
        }
    };

    if principal
        .as_ref()
        .is_some_and(|Extension(key)| !key.allows(&requested_model))
    {
        return model_forbidden();
    }
    let model = match state.model(&requested_model).await {
        Some(model) => model,
        None => {
            return auth_error(
                StatusCode::NOT_FOUND,
                "model_not_found",
                format!("model '{requested_model}' is not configured"),
                Some("model"),
            )
        }
    };

    let streamed = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    apply_model_parameters(&mut payload, &model.parameters);
    if streamed {
        enable_stream_usage(&mut payload, &model);
    }

    let endpoint = chat_completions_endpoint(&model.api_base);
    let request_id = Uuid::new_v4().to_string();
    // Covers the request-send phase. A streaming response additionally hands its
    // own guard to the body, which keeps the request counted until the stream ends.
    let _active_request = ActiveRequestGuard::new(state.active_requests.clone());

    let mut context = RequestContext {
        virtual_key_id: principal.as_ref().map(|Extension(key)| key.key_id.clone()),
        daemon_id: state.settings.daemon_id.clone(),
        model_name: requested_model.clone(),
        request_id: request_id.clone(),
        streamed,
        started: Instant::now(),
        status_code: None,
    };

    let mut request = state.client.post(endpoint).json(&payload);
    let provider_key = if state.settings.auth_mode == AuthMode::VirtualKeys {
        match model.parameters.get("api_key_env") {
            Some(name) => match std::env::var(name) {
                Ok(key) if !key.trim().is_empty() => Some(key),
                _ => {
                    warn!(model = %requested_model, "configured upstream credential is unavailable");
                    return auth_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "upstream_auth_unavailable",
                        "upstream credential is unavailable",
                        None,
                    );
                }
            },
            None => model.parameters.get("api_key").cloned(),
        }
    } else {
        configured_api_key(&model.parameters)
    };
    if let Some(api_key) = provider_key {
        request = request.bearer_auth(api_key);
    } else if state.settings.auth_mode == AuthMode::Legacy {
        if let Some(authorization) = headers.get(header::AUTHORIZATION) {
            request = request.header(header::AUTHORIZATION, authorization.clone());
        }
    }
    if !streamed {
        // Streaming responses are bounded by the client read timeout instead.
        request = request.timeout(state.settings.request_timeout);
    }

    let upstream = match request.send().await {
        Ok(response) => response,
        Err(err) => {
            let timed_out = err.is_timeout();
            warn!(model = %requested_model, timed_out, error = %err, "upstream request failed");
            state.report(context.telemetry(None, false));
            return if timed_out {
                openai_error(StatusCode::GATEWAY_TIMEOUT, "upstream provider timed out")
            } else {
                openai_error(StatusCode::BAD_GATEWAY, "upstream provider is unavailable")
            };
        }
    };

    let status = upstream.status();
    let content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .or_else(|| streamed.then(|| HeaderValue::from_static("text/event-stream")));
    let cache_control = upstream.headers().get(header::CACHE_CONTROL).cloned();
    context.status_code = Some(status.as_u16());

    let mut builder = Response::builder()
        .status(status)
        .header("x-daemon-request-id", request_id);
    if let Some(value) = content_type {
        builder = builder.header(header::CONTENT_TYPE, value);
    }
    if let Some(value) = cache_control {
        builder = builder.header(header::CACHE_CONTROL, value);
    }

    if streamed {
        let guard = TelemetryGuard::new(
            state.clone(),
            context,
            ActiveRequestGuard::new(state.active_requests.clone()),
        );
        let body = upstream::streaming_body(guard, upstream.bytes_stream());
        return builder
            .body(Body::from_stream(body))
            .unwrap_or_else(|_| internal_error("failed to build streaming response"));
    }

    // Non-streaming responses are buffered so provider-reported usage can be read
    // off the body before it is forwarded.
    let body = match read_capped(upstream, state.settings.max_response_bytes).await {
        Ok(body) => body,
        Err(err) => {
            warn!(model = %requested_model, error = %err, "failed to read upstream response");
            state.report(context.telemetry(None, false));
            return openai_error(
                StatusCode::BAD_GATEWAY,
                "upstream response could not be read",
            );
        }
    };

    let usage = status
        .is_success()
        .then(|| Usage::from_body(&body))
        .flatten();
    state.report(context.telemetry(usage, status.is_success()));

    builder
        .body(Body::from(body))
        .unwrap_or_else(|_| internal_error("failed to build response"))
}

async fn read_capped(response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| err.to_string())?;
        if body.len() + chunk.len() > limit {
            return Err(format!("upstream response exceeded {limit} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn model_forbidden() -> Response {
    warn!(
        code = "model_not_allowed",
        "inference authorization rejected"
    );
    auth_error(
        StatusCode::FORBIDDEN,
        "model_not_allowed",
        "the virtual key cannot access this model",
        Some("model"),
    )
}

fn auth_error(
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
    param: Option<&str>,
) -> Response {
    let error_type = match status {
        StatusCode::UNAUTHORIZED => "authentication_error",
        StatusCode::FORBIDDEN => "permission_error",
        StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND => "invalid_request_error",
        _ => "server_error",
    };
    let mut response = (
        status,
        Json(json!({"error": {
            "message": message.into(), "type": error_type, "param": param, "code": code
        }})),
    )
        .into_response();
    if status == StatusCode::UNAUTHORIZED {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    response
}

fn internal_error(message: &str) -> Response {
    openai_error(StatusCode::INTERNAL_SERVER_ERROR, message)
}

fn openai_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "message": message.into(),
                "type": "daemon_error",
                "param": null,
                "code": status.as_u16()
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DaemonSettings;
    use axum::body::{to_bytes, Bytes};
    use axum::routing::post;
    use protocol::{DaemonMessage, ModelConfigParameters};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    const UPSTREAM_MODEL: &str = "demo-model";

    fn settings(api_keys: Vec<String>, request_timeout: Duration) -> DaemonSettings {
        DaemonSettings {
            control_token: None,
            auth_mode: AuthMode::Legacy,
            daemon_id: "test-daemon".to_string(),
            api_keys,
            connect_timeout: Duration::from_secs(2),
            read_timeout: Duration::from_secs(5),
            request_timeout,
            max_response_bytes: 1024 * 1024,
        }
    }

    async fn spawn(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }

    async fn harness_with(
        upstream: Router,
        api_keys: Vec<String>,
        request_timeout: Duration,
    ) -> (Router, mpsc::Receiver<DaemonMessage>) {
        let (_state, app, rx) = harness_state(upstream, api_keys, request_timeout).await;
        (app, rx)
    }

    async fn harness_state(
        upstream: Router,
        api_keys: Vec<String>,
        request_timeout: Duration,
    ) -> (AppState, Router, mpsc::Receiver<DaemonMessage>) {
        let base = spawn(upstream).await;
        let (tx, rx) = mpsc::channel(16);
        let state = AppState::new(settings(api_keys, request_timeout), tx).expect("client builds");
        state
            .set_models(vec![ModelConfig {
                model_name: UPSTREAM_MODEL.to_string(),
                provider: "custom_openai".to_string(),
                api_base: format!("{base}/v1"),
                parameters: ModelConfigParameters::new(),
            }])
            .await;
        let app = create_app(state.clone());
        (state, app, rx)
    }

    async fn harness(upstream: Router) -> (Router, mpsc::Receiver<DaemonMessage>) {
        harness_with(upstream, Vec::new(), Duration::from_secs(5)).await
    }

    async fn call(app: Router, body: Value) -> Response {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    async fn next_telemetry(rx: &mut mpsc::Receiver<DaemonMessage>) -> protocol::Telemetry {
        let message = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("telemetry should arrive")
            .expect("channel should stay open");
        match message {
            DaemonMessage::Telemetry(telemetry) => telemetry,
            other => panic!("expected telemetry, got {other:?}"),
        }
    }

    fn virtual_token() -> String {
        format!("sk-{}_{}", "a".repeat(32), "b".repeat(64))
    }

    async fn virtual_harness(
        upstream: Router,
    ) -> (AppState, Router, mpsc::Receiver<DaemonMessage>) {
        let base = spawn(upstream).await;
        let mut settings = settings(Vec::new(), Duration::from_secs(5));
        settings.auth_mode = AuthMode::VirtualKeys;
        let (tx, rx) = mpsc::channel(16);
        let state = AppState::new(settings, tx).unwrap();
        state
            .set_models(vec![
                ModelConfig {
                    model_name: UPSTREAM_MODEL.into(),
                    provider: "custom_openai".into(),
                    api_base: format!("{base}/v1"),
                    parameters: ModelConfigParameters::new(),
                },
                ModelConfig {
                    model_name: "restricted".into(),
                    provider: "custom_openai".into(),
                    api_base: format!("{base}/v1"),
                    parameters: ModelConfigParameters::new(),
                },
            ])
            .await;
        let app = create_app(state.clone());
        (state, app, rx)
    }

    async fn grant(state: &AppState, revision: i64, keys: bool) {
        use sha2::{Digest, Sha256};
        let snapshot = protocol::VirtualKeyPolicySnapshot {
            protocol_version: 1,
            revision,
            issued_at: now_ts(),
            valid_until: now_ts() + 300,
            keys: if keys {
                vec![protocol::VirtualKeyPolicy {
                    key_id: "a".repeat(32),
                    token_sha256: format!("{:x}", Sha256::digest(virtual_token().as_bytes())),
                    allowed_models: vec![UPSTREAM_MODEL.into(), "removed".into()],
                    expires_at: None,
                }]
            } else {
                vec![]
            },
        };
        state
            .policies
            .write()
            .await
            .apply(snapshot, now_ts(), tokio::time::Instant::now())
            .unwrap();
    }

    async fn virtual_call(app: Router, method: &str, path: &str, body: Value) -> Response {
        app.oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {}", virtual_token()))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn virtual_keys_fail_closed_filter_models_and_enforce_revocation() {
        let (state, app, mut rx) = virtual_harness(Router::new()).await;
        let unavailable = virtual_call(app.clone(), "GET", "/v1/models", Value::Null).await;
        assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
        let missing = call(app.clone(), json!({"model": UPSTREAM_MODEL})).await;
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(missing.headers()["www-authenticate"], "Bearer");
        grant(&state, 1, true).await;
        let list = virtual_call(app.clone(), "GET", "/v1/models", Value::Null).await;
        assert_eq!(list.status(), StatusCode::OK);
        let value: Value =
            serde_json::from_slice(&to_bytes(list.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert_eq!(value["data"].as_array().unwrap().len(), 1);
        assert_eq!(value["data"][0]["id"], UPSTREAM_MODEL);
        for (method, path, payload, status) in [
            (
                "GET",
                "/v1/models/restricted",
                Value::Null,
                StatusCode::FORBIDDEN,
            ),
            (
                "POST",
                "/v1/chat/completions",
                json!({"model":"restricted"}),
                StatusCode::FORBIDDEN,
            ),
            (
                "GET",
                "/v1/models/removed",
                Value::Null,
                StatusCode::NOT_FOUND,
            ),
            (
                "POST",
                "/v1/chat/completions",
                json!({}),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            assert_eq!(
                virtual_call(app.clone(), method, path, payload)
                    .await
                    .status(),
                status
            );
        }
        grant(&state, 2, false).await;
        assert_eq!(
            virtual_call(app, "GET", "/v1/models", Value::Null)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert!(
            rx.try_recv().is_err(),
            "admission failures must not produce upstream usage"
        );
    }

    #[tokio::test]
    async fn virtual_key_never_reaches_upstream_and_usage_has_key_id() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(
                |headers: HeaderMap, Json(payload): Json<Value>| async move {
                    assert!(headers.get(header::AUTHORIZATION).is_none());
                    assert_eq!(payload["model"], "real-provider-model");
                    Json(json!({"usage":{"prompt_tokens":1,"completion_tokens":2}}))
                },
            ),
        );
        let (state, app, mut rx) = virtual_harness(upstream).await;
        state
            .models
            .write()
            .await
            .get_mut(UPSTREAM_MODEL)
            .unwrap()
            .parameters
            .insert("upstream_model", "real-provider-model");
        grant(&state, 1, true).await;
        let response = virtual_call(
            app,
            "POST",
            "/v1/chat/completions",
            json!({"model":UPSTREAM_MODEL}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let event = next_telemetry(&mut rx).await;
        assert_eq!(
            event.virtual_key_id.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
    }

    #[tokio::test]
    async fn virtual_mode_does_not_fall_back_when_provider_env_is_missing() {
        let (state, app, mut rx) = virtual_harness(Router::new()).await;
        {
            let mut models = state.models.write().await;
            let model = models.get_mut(UPSTREAM_MODEL).unwrap();
            model
                .parameters
                .insert("api_key_env", "ONEFLOW_TEST_MISSING_PROVIDER_CREDENTIAL");
            model.parameters.insert("api_key", "must-not-fallback");
        }
        grant(&state, 1, true).await;
        let response = virtual_call(
            app,
            "POST",
            "/v1/chat/completions",
            json!({"model":UPSTREAM_MODEL}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["error"]["code"], "upstream_auth_unavailable");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn virtual_stream_uses_provider_key_and_survives_later_revocation() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|headers: HeaderMap, Json(payload): Json<Value>| async move {
                assert_eq!(headers[header::AUTHORIZATION], "Bearer provider-test-token");
                assert_eq!(payload["stream_options"]["include_usage"], true);
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from("data: {\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":4}}\n\ndata: [DONE]\n\n"))
                    .unwrap()
            }),
        );
        let (state, app, mut rx) = virtual_harness(upstream).await;
        state
            .models
            .write()
            .await
            .get_mut(UPSTREAM_MODEL)
            .unwrap()
            .parameters
            .insert("api_key", "provider-test-token");
        grant(&state, 1, true).await;
        let response = virtual_call(
            app.clone(),
            "POST",
            "/v1/chat/completions",
            json!({"model":UPSTREAM_MODEL,"stream":true}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        grant(&state, 2, false).await;
        assert_eq!(
            virtual_call(app, "GET", "/v1/models", Value::Null)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(std::str::from_utf8(&body).unwrap().contains("[DONE]"));
        let event = next_telemetry(&mut rx).await;
        assert!(event.streamed && event.success);
        assert_eq!(event.completion_tokens, 4);
        assert!(event.virtual_key_id.is_some());
    }

    #[tokio::test]
    async fn non_streaming_reports_provider_usage() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|Json(_payload): Json<Value>| async {
                Json(json!({
                    "id": "chatcmpl-1",
                    "object": "chat.completion",
                    "usage": {"prompt_tokens": 11, "completion_tokens": 7},
                }))
            }),
        );
        let (app, mut rx) = harness(upstream).await;

        let response = call(app, json!({"model": UPSTREAM_MODEL, "messages": []})).await;
        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["usage"]["prompt_tokens"], 11, "body passes through");

        let telemetry = next_telemetry(&mut rx).await;
        assert_eq!(telemetry.prompt_tokens, 11);
        assert_eq!(telemetry.completion_tokens, 7);
        assert!(telemetry.success);
        assert!(telemetry.usage_reported);
        assert_eq!(telemetry.status_code, Some(200));
        assert!(!telemetry.streamed);
        assert_eq!(telemetry.model_name, UPSTREAM_MODEL);
        assert!(!telemetry.request_id.is_empty());
    }

    #[tokio::test]
    async fn streaming_reports_usage_split_across_chunks_and_injects_stream_options() {
        let seen_payload: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let captured = seen_payload.clone();

        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(payload): Json<Value>| {
                let captured = captured.clone();
                async move {
                    *captured.lock().unwrap() = Some(payload);
                    // The usage frame is deliberately split mid-JSON across chunks.
                    let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
                        Ok(Bytes::from_static(
                            b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
                        )),
                        Ok(Bytes::from_static(b"data: {\"us")),
                        Ok(Bytes::from_static(
                            b"age\":{\"prompt_tokens\":3,\"completion_tokens\":4}}\n\ndata: [DONE]\n\n",
                        )),
                    ];
                    Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(Body::from_stream(futures_util::stream::iter(chunks)))
                        .unwrap()
                }
            }),
        );
        let (app, mut rx) = harness(upstream).await;

        let response = call(
            app,
            json!({"model": UPSTREAM_MODEL, "stream": true, "messages": []}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/event-stream"
        );

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            text.contains("[DONE]"),
            "client must receive the raw stream"
        );
        assert_eq!(text.matches("data:").count(), 3);

        let payload = seen_payload.lock().unwrap().clone().unwrap();
        assert_eq!(payload["stream_options"]["include_usage"], true);

        let telemetry = next_telemetry(&mut rx).await;
        assert_eq!(telemetry.prompt_tokens, 3);
        assert_eq!(telemetry.completion_tokens, 4);
        assert!(telemetry.streamed);
        assert!(telemetry.usage_reported);
        assert!(telemetry.success);
        assert_eq!(telemetry.status_code, Some(200));
    }

    #[tokio::test]
    async fn upstream_error_is_reported_as_failure() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "upstream exploded"})),
                )
            }),
        );
        let (app, mut rx) = harness(upstream).await;

        let response = call(app, json!({"model": UPSTREAM_MODEL, "messages": []})).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let telemetry = next_telemetry(&mut rx).await;
        assert!(!telemetry.success);
        assert!(!telemetry.usage_reported);
        assert_eq!(telemetry.status_code, Some(500));
        assert_eq!(telemetry.prompt_tokens, 0);
    }

    #[tokio::test]
    async fn slow_upstream_is_reported_as_a_timeout() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Json(json!({"id": "too-late"}))
            }),
        );
        let (app, mut rx) = harness_with(upstream, Vec::new(), Duration::from_millis(200)).await;

        let response = call(app, json!({"model": UPSTREAM_MODEL, "messages": []})).await;
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);

        let telemetry = next_telemetry(&mut rx).await;
        assert!(!telemetry.success);
        assert_eq!(telemetry.status_code, None, "no response was received");
    }

    #[tokio::test]
    async fn missing_api_key_is_rejected_before_reaching_the_upstream() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|| async { Json(json!({"id": "should-not-be-called"})) }),
        );
        let (app, mut rx) = harness_with(
            upstream,
            vec!["secret-key".to_string()],
            Duration::from_secs(5),
        )
        .await;

        let response = call(app, json!({"model": UPSTREAM_MODEL, "messages": []})).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get(header::WWW_AUTHENTICATE).unwrap(),
            "Bearer"
        );

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], 401);
        assert!(
            rx.try_recv().is_err(),
            "a rejected request must not produce telemetry"
        );
    }

    #[tokio::test]
    async fn streaming_request_stays_active_until_the_body_is_consumed() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
                    Ok(Bytes::from_static(b"data: {\"choices\":[]}\n\n")),
                    Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                ];
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from_stream(futures_util::stream::iter(chunks)))
                    .unwrap()
            }),
        );
        let (state, app, _rx) = harness_state(upstream, Vec::new(), Duration::from_secs(5)).await;

        let response = call(
            app,
            json!({"model": UPSTREAM_MODEL, "stream": true, "messages": []}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            state.active_requests.load(Ordering::Relaxed),
            1,
            "the guard must outlive the handler while the body is still streaming"
        );

        let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            state.active_requests.load(Ordering::Relaxed),
            0,
            "the guard must be released once the stream ends"
        );
    }

    #[tokio::test]
    async fn health_and_model_lookup() {
        let upstream = Router::new();
        let (app, _rx) = harness(upstream).await;

        let health = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        let body = to_bytes(health.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["status"], "ok");
        assert_eq!(value["models"], 1);

        let found = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/models/{UPSTREAM_MODEL}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(found.status(), StatusCode::OK);
        let body = to_bytes(found.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["id"], UPSTREAM_MODEL);
        assert!(value["created"].as_i64().unwrap() > 0);

        let missing = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models/nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }
}
