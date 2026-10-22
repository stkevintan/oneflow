//! LiteLLM-style key-management subset; unsupported policies are rejected.
use crate::virtual_keys::{CreateVirtualKey, VirtualKey};
use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Query, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    api::{error, publish_policy_hint},
    now_ts, AppState,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GenerateParams {
    key_alias: Option<String>,
    models: Vec<String>,
    duration: Option<String>,
}

pub(crate) fn duration_seconds(value: &str) -> Option<i64> {
    let (digits, multiplier) = match value.as_bytes().last()? {
        b's' => (&value[..value.len() - 1], 1),
        b'm' => (&value[..value.len() - 1], 60),
        b'h' => (&value[..value.len() - 1], 3600),
        b'd' => (&value[..value.len() - 1], 86400),
        _ => return None,
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits
        .parse::<i64>()
        .ok()?
        .checked_mul(multiplier)
        .filter(|seconds| *seconds > 0)
}

fn timestamp(value: Option<i64>) -> anyhow::Result<Option<String>> {
    value
        .map(|value| {
            let date = chrono::DateTime::from_timestamp(value, 0)
                .ok_or_else(|| anyhow::anyhow!("stored key timestamp is outside RFC 3339 range"))?;
            Ok(date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        })
        .transpose()
}

fn metadata(key: &VirtualKey, hash: &str) -> anyhow::Result<Value> {
    Ok(json!({
        "token": hash,
        "key_alias": key.name,
        "models": key.allowed_models,
        "expires": timestamp(key.expires_at)?,
        "created_at": timestamp(Some(key.created_at))?,
        "blocked": key.revoked_at.is_some(),
        "token_id": key.id
    }))
}

fn unavailable() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "virtual-key storage is unavailable",
    )
}

pub(crate) async fn generate(
    State(state): State<AppState>,
    payload: Result<Json<GenerateParams>, JsonRejection>,
) -> Response {
    let mode = &state.access;
    let Json(params) = match payload {
        Ok(params) => params,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid key generation request or unsupported fields",
            )
        }
    };
    let now = now_ts();
    let expiry = match params.duration.as_deref() {
        None => None,
        Some(duration) => match duration_seconds(duration)
            .and_then(|seconds| now.checked_add(seconds))
            .filter(|expiry| chrono::DateTime::from_timestamp(*expiry, 0).is_some())
        {
            Some(expiry) => Some(expiry),
            None => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "duration must be a positive integer followed by s, m, h, or d",
                )
            }
        },
    };
    let request = CreateVirtualKey {
        name: params
            .key_alias
            .unwrap_or_else(|| format!("key-{}", uuid::Uuid::new_v4().simple())),
        allowed_models: params.models,
        expires_at: expiry,
    };
    match mode.store.create(request, now).await {
        Ok(created) => {
            use sha2::{Digest, Sha256};
            let hash = hex::encode(Sha256::digest(created.secret.as_bytes()));
            let mut response = match metadata(&created.key, &hash) {
                Ok(response) => response,
                Err(err) => {
                    tracing::error!(error = %err, "created key metadata is invalid");
                    return unavailable();
                }
            };
            response["key"] = Value::String(created.secret);
            publish_policy_hint(&state, mode).await;
            Json(response).into_response()
        }
        Err(err)
            if err
                .chain()
                .any(|cause| cause.downcast_ref::<sqlx::Error>().is_some()) =>
        {
            tracing::error!(error = %err, "key generation storage failure");
            unavailable()
        }
        Err(err) => error(StatusCode::BAD_REQUEST, err.to_string()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InfoParams {
    key: String,
}

pub(crate) async fn info(
    State(state): State<AppState>,
    params: Result<Query<InfoParams>, QueryRejection>,
) -> Response {
    let mode = &state.access;
    let Query(params) = match params {
        Ok(params) => params,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "key query parameter is required; use its SHA-256 hash",
            )
        }
    };
    let result: anyhow::Result<Option<Value>> = async {
        let Some(id) = mode.store.resolve_hash(&params.key).await? else {
            return Ok(None);
        };
        let key = mode.store.get(&id).await?;
        let hash = mode.store.key_hash(&id).await?;
        key.map(|key| Ok(json!({"key": hash, "info": metadata(&key, &hash)?})))
            .transpose()
    }
    .await;
    match result {
        Ok(Some(value)) => Json(value).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "key not found"),
        Err(err) => {
            tracing::error!(error = %err, "key lookup storage failure");
            unavailable()
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListParams {
    page: Option<i64>,
    size: Option<i64>,
    return_full_object: Option<bool>,
}

pub(crate) async fn list(
    State(state): State<AppState>,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Response {
    let mode = &state.access;
    let Query(params) = match params {
        Ok(params) => params,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid pagination or unsupported query fields",
            )
        }
    };
    let page = params.page.unwrap_or(1);
    let size = params.size.unwrap_or(10);
    if page < 1 || !(1..=100).contains(&size) || (page - 1).checked_mul(size).is_none() {
        return error(
            StatusCode::BAD_REQUEST,
            "page must be positive and size must be between 1 and 100",
        );
    }
    match mode.store.list_page(page, size).await {
        Ok((keys, total)) => {
            let keys: anyhow::Result<Vec<Value>> = keys
                .iter()
                .map(|(key, hash)| {
                    if params.return_full_object.unwrap_or(false) {
                        metadata(key, hash)
                    } else {
                        Ok(Value::String(hash.clone()))
                    }
                })
                .collect();
            let keys = match keys {
                Ok(keys) => keys,
                Err(err) => {
                    tracing::error!(error = %err, "stored key metadata is invalid");
                    return unavailable();
                }
            };
            Json(
                json!({"keys": keys, "total_count": total, "current_page": page,
                "total_pages": total / size + i64::from(total % size != 0)}),
            )
            .into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, "key list storage failure");
            unavailable()
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteParams {
    keys: Vec<String>,
}

pub(crate) async fn delete(
    State(state): State<AppState>,
    payload: Result<Json<DeleteParams>, JsonRejection>,
) -> Response {
    let mode = &state.access;
    let Json(params) = match payload {
        Ok(params) => params,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "keys must be an array; unsupported fields are rejected",
            )
        }
    };
    if params.keys.is_empty() || params.keys.len() > 100 {
        return error(
            StatusCode::BAD_REQUEST,
            "keys must contain 1 to 100 identifiers",
        );
    }
    match mode.store.delete_keys(&params.keys, now_ts()).await {
        Ok(true) => {
            let deleted: Vec<String> = params
                .keys
                .iter()
                .map(|key| crate::virtual_keys::identifier_hash(key))
                .collect();
            publish_policy_hint(&state, mode).await;
            Json(json!({"deleted_keys": deleted})).into_response()
        }
        Ok(false) => error(
            StatusCode::NOT_FOUND,
            "one or more keys were not found; no keys deleted",
        ),
        Err(err) => {
            tracing::error!(error = %err, "key deletion storage failure");
            unavailable()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_reject_invalid_stored_values() {
        assert_eq!(timestamp(None).unwrap(), None);
        assert_eq!(
            timestamp(Some(0)).unwrap().as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
        assert!(timestamp(Some(i64::MAX)).is_err());
    }
    #[test]
    fn durations_and_unsupported_policies_are_strict() {
        assert_eq!(duration_seconds("30d"), Some(2592000));
        for value in ["0s", "-1h", "2weeks", "999999999999999999d", "1.5h"] {
            assert_eq!(duration_seconds(value), None);
        }
        assert!(serde_json::from_value::<GenerateParams>(
            json!({"models":["alias"],"max_budget":1})
        )
        .is_err());
        assert!(serde_json::from_value::<GenerateParams>(
            json!({"models":["alias"],"key_alias":"app","duration":"1h"})
        )
        .is_ok());
    }

    #[tokio::test]
    async fn litellm_key_lifecycle_with_postgres() {
        use axum::{
            body::{to_bytes, Body},
            http::{header, Request},
        };
        use sha2::{Digest, Sha256};
        use std::sync::Arc;
        use tower::ServiceExt;
        let Ok(url) = std::env::var("ONEFLOW_TEST_DATABASE_URL") else {
            return;
        };
        let store = Arc::new(
            crate::virtual_keys::VirtualKeyStore::connect(&url, &crate::store::default_config())
                .await
                .unwrap(),
        );
        let model = store.config().await.unwrap().models[0].model_name.clone();
        let (broadcast, _) = tokio::sync::broadcast::channel(8);
        let state = AppState {
            store: Arc::new(crate::store::MemoryStore::new(
                crate::store::default_config(),
            )),
            broadcast,
            started_at: now_ts(),
            access: Arc::new(crate::AccessControl {
                store: store.clone(),
                admin_token: Arc::new("test-admin-token".into()),
                control_plane_url: "wss://control.example/control".into(),
            }),
        };
        let app = crate::api::create_app(state, vec![]);
        let request = |method: &str, path: &str, payload: Value| {
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, "Bearer test-admin-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap()
        };
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/key/list")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
        for payload in [
            json!({"models":[]}),
            json!({"models":[model],"max_budget":5}),
            json!({"models":[model],"duration":"0h"}),
            json!({"models":["not-configured"]}),
        ] {
            let response = app
                .clone()
                .oneshot(request("POST", "/key/generate", payload))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/key/generate",
                json!({
                    "models":[model], "key_alias":"litellm-app", "duration":"1h"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let created: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        let secret = created["key"].as_str().unwrap();
        assert!(secret.starts_with("sk-"));
        assert!(created["expires"].as_str().unwrap().ends_with('Z'));
        assert_eq!(created["models"], json!([model]));
        let hash = hex::encode(Sha256::digest(secret.as_bytes()));
        assert_eq!(created["token"], hash);
        let id = created["token_id"].as_str().unwrap();
        for identifier in [secret, hash.as_str()] {
            let response = app
                .clone()
                .oneshot(request(
                    "GET",
                    &format!("/key/info?key={identifier}"),
                    Value::Null,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                    .unwrap();
            assert_eq!(body["key"], hash);
            assert_eq!(body["info"]["key_alias"], "litellm-app");
            assert!(!body.to_string().contains(secret));
        }
        for full in [false, true] {
            let response = app
                .clone()
                .oneshot(request(
                    "GET",
                    &format!("/key/list?page=1&size=100&return_full_object={full}"),
                    Value::Null,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                    .unwrap();
            assert_eq!(body["current_page"], 1);
            assert!(body["total_count"].as_i64().unwrap() > 0);
            let keys = body["keys"].as_array().unwrap();
            assert!(keys.iter().all(|key| if full {
                key["token"].is_string()
            } else {
                key.is_string()
            }));
            assert!(!body.to_string().contains(secret));
        }
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/key/delete",
                json!({"keys":[hash,"unknown"]}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            store.get(id).await.unwrap().unwrap().revoked_at.is_none(),
            "batch is atomic"
        );
        for identifier in [secret, hash.as_str()] {
            let response = app
                .clone()
                .oneshot(request("POST", "/key/delete", json!({"keys":[identifier]})))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                    .unwrap();
            assert_eq!(body["deleted_keys"], json!([hash]));
        }
        assert!(store.get(id).await.unwrap().unwrap().revoked_at.is_some());
        assert!(!store
            .snapshot(now_ts())
            .await
            .unwrap()
            .keys
            .iter()
            .any(|policy| policy.key_id == id));
        let response = app
            .clone()
            .oneshot(request("GET", "/key/list?size=0", Value::Null))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = app
            .oneshot(request("POST", "/v1/virtual-keys", json!({})))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
