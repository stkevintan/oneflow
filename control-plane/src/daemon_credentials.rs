use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use protocol::{DaemonCredential, DaemonCredentialList, IssuedDaemonCredential};
use rand::{rngs::OsRng, RngCore};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::Row;

use crate::{api::error, now_ts, virtual_keys::VirtualKeyStore, AppState};

#[derive(Clone)]
pub(crate) struct AuthenticatedDaemon {
    pub(crate) daemon_id: String,
    pub(crate) token_hash: String,
}

pub(crate) fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn new_token() -> String {
    let mut secret = [0u8; 32];
    OsRng.fill_bytes(&mut secret);
    format!("ofd_{}", hex::encode(secret))
}

fn credential(row: sqlx::postgres::PgRow) -> DaemonCredential {
    DaemonCredential {
        daemon_id: row.get("daemon_id"),
        created_at: row.get("created_at"),
        rotated_at: row.get("rotated_at"),
        revoked_at: row.get("revoked_at"),
    }
}

impl VirtualKeyStore {
    pub(crate) async fn create_daemon(
        &self,
        daemon_id: &str,
        now: i64,
    ) -> anyhow::Result<Option<IssuedDaemonCredential>> {
        let token = new_token();
        let row = sqlx::query(
            "INSERT INTO daemon_credentials (daemon_id, token_sha256, created_at) \
             VALUES ($1, $2, $3) ON CONFLICT (daemon_id) DO NOTHING \
             RETURNING daemon_id, created_at, rotated_at, revoked_at",
        )
        .bind(daemon_id)
        .bind(token_hash(&token))
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| IssuedDaemonCredential {
            credential: credential(row),
            token,
        }))
    }

    pub(crate) async fn daemon_credentials(&self) -> anyhow::Result<Vec<DaemonCredential>> {
        Ok(sqlx::query(
            "SELECT daemon_id, created_at, rotated_at, revoked_at FROM daemon_credentials ORDER BY daemon_id",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(credential)
        .collect())
    }

    pub(crate) async fn rotate_daemon(
        &self,
        daemon_id: &str,
        now: i64,
    ) -> anyhow::Result<Option<IssuedDaemonCredential>> {
        let token = new_token();
        let row = sqlx::query(
            "UPDATE daemon_credentials SET token_sha256 = $2, rotated_at = $3, revoked_at = NULL \
             WHERE daemon_id = $1 RETURNING daemon_id, created_at, rotated_at, revoked_at",
        )
        .bind(daemon_id)
        .bind(token_hash(&token))
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| IssuedDaemonCredential {
            credential: credential(row),
            token,
        }))
    }

    pub(crate) async fn revoke_daemon(
        &self,
        daemon_id: &str,
        now: i64,
    ) -> anyhow::Result<Option<DaemonCredential>> {
        Ok(sqlx::query(
            "UPDATE daemon_credentials SET revoked_at = COALESCE(revoked_at, $2) \
             WHERE daemon_id = $1 RETURNING daemon_id, created_at, rotated_at, revoked_at",
        )
        .bind(daemon_id)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?
        .map(credential))
    }

    pub(crate) async fn daemon_credential_valid(
        &self,
        identity: &AuthenticatedDaemon,
    ) -> anyhow::Result<bool> {
        let expected: Option<String> = sqlx::query_scalar(
            "SELECT token_sha256 FROM daemon_credentials WHERE daemon_id = $1 AND revoked_at IS NULL",
        )
        .bind(&identity.daemon_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(expected.is_some_and(|hash| crate::constant_time_token_eq(&hash, &identity.token_hash)))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateParams {
    daemon_id: String,
}

fn valid_daemon_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn unavailable() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "daemon credential storage is unavailable; configure database_url and admin_token",
    )
}

fn storage_error(err: anyhow::Error) -> Response {
    tracing::error!(error = %err, "daemon credential storage failure");
    unavailable()
}

pub(crate) async fn create(
    State(state): State<AppState>,
    payload: Result<Json<CreateParams>, JsonRejection>,
) -> Response {
    let Some(mode) = &state.virtual_key_mode else {
        return unavailable();
    };
    let Json(params) = match payload {
        Ok(params) => params,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "expected daemon_id; unsupported fields are rejected",
            )
        }
    };
    if !valid_daemon_id(&params.daemon_id) {
        return error(
            StatusCode::BAD_REQUEST,
            "daemon_id must be 1-128 ASCII letters, digits, dots, underscores, or hyphens",
        );
    }
    match mode.store.create_daemon(&params.daemon_id, now_ts()).await {
        Ok(Some(issued)) => (StatusCode::CREATED, Json(issued)).into_response(),
        Ok(None) => error(
            StatusCode::CONFLICT,
            "daemon_id already exists; rotate its credential instead",
        ),
        Err(err) => storage_error(err),
    }
}

pub(crate) async fn list(State(state): State<AppState>) -> Response {
    let Some(mode) = &state.virtual_key_mode else {
        return unavailable();
    };
    match mode.store.daemon_credentials().await {
        Ok(items) => Json(DaemonCredentialList { items }).into_response(),
        Err(err) => storage_error(err),
    }
}

pub(crate) async fn rotate(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(mode) = &state.virtual_key_mode else {
        return unavailable();
    };
    match mode.store.rotate_daemon(&id, now_ts()).await {
        Ok(Some(issued)) => Json(issued).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "daemon credential not found"),
        Err(err) => storage_error(err),
    }
}

pub(crate) async fn revoke(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(mode) = &state.virtual_key_mode else {
        return unavailable();
    };
    match mode.store.revoke_daemon(&id, now_ts()).await {
        Ok(Some(credential)) => Json(credential).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "daemon credential not found"),
        Err(err) => storage_error(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request},
        Router,
    };
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{json, Value};
    use std::{sync::Arc, time::Duration};
    use tokio::sync::broadcast;
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
    use tower::ServiceExt;

    async fn call(
        app: &Router,
        method: &str,
        path: &str,
        payload: Value,
        authorized: bool,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json");
        if authorized {
            builder = builder.header(header::AUTHORIZATION, "Bearer test-admin-token");
        }
        let response = app
            .clone()
            .oneshot(builder.body(Body::from(payload.to_string())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let status = response.status();
        let body =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        (status, body)
    }

    #[test]
    fn validates_daemon_identifiers() {
        for id in ["", "bad/id", "has space", "é"] {
            assert!(!valid_daemon_id(id));
        }
        assert!(!valid_daemon_id(&"a".repeat(129)));
        assert!(valid_daemon_id(&"a".repeat(128)));
        assert!(valid_daemon_id("server-A_1.example"));
    }

    #[tokio::test]
    async fn postgres_provisioning_rotation_revocation_and_control_fencing() {
        let Ok(url) = std::env::var("ONEFLOW_TEST_DATABASE_URL") else {
            return;
        };
        let initial = crate::store::default_config();
        let store = Arc::new(VirtualKeyStore::connect(&url, &initial).await.unwrap());
        let second = Arc::new(VirtualKeyStore::connect(&url, &initial).await.unwrap());
        let (broadcast, _) = broadcast::channel(8);
        let state = AppState {
            store: Arc::new(crate::store::MemoryStore::new(initial)),
            broadcast,
            started_at: now_ts(),
            virtual_key_mode: Some(Arc::new(crate::VirtualKeyMode {
                store: store.clone(),
                admin_token: Arc::new("test-admin-token".into()),
            })),
        };
        let app = crate::api::create_app(state.clone(), vec![]);
        let id = format!("daemon-test-{}", uuid::Uuid::new_v4().simple());
        for (method, path) in [
            ("POST", "/v1/daemons".to_string()),
            ("GET", "/v1/daemons/credentials".to_string()),
            ("POST", format!("/v1/daemons/{id}/rotate")),
            ("POST", format!("/v1/daemons/{id}/revoke")),
        ] {
            assert_eq!(
                call(&app, method, &path, json!({}), false).await.0,
                StatusCode::UNAUTHORIZED
            );
        }
        for payload in [
            json!({}),
            json!({"daemon_id":"bad/id"}),
            json!({"daemon_id":id,"token":"caller-secret"}),
        ] {
            assert_eq!(
                call(&app, "POST", "/v1/daemons", payload, true).await.0,
                StatusCode::BAD_REQUEST
            );
        }
        let (status, created) =
            call(&app, "POST", "/v1/daemons", json!({"daemon_id":id}), true).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created["credential"]["daemon_id"], id);
        let token = created["token"].as_str().unwrap();
        assert_eq!(token.len(), 68);
        let identity = AuthenticatedDaemon {
            daemon_id: id.clone(),
            token_hash: token_hash(token),
        };
        assert!(second.daemon_credential_valid(&identity).await.unwrap());
        let persisted: String =
            sqlx::query_scalar("SELECT token_sha256 FROM daemon_credentials WHERE daemon_id = $1")
                .bind(&id)
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(persisted, token_hash(token));
        assert_eq!(
            call(&app, "POST", "/v1/daemons", json!({"daemon_id":id}), true)
                .await
                .0,
            StatusCode::CONFLICT
        );
        let (status, listing) =
            call(&app, "GET", "/v1/daemons/credentials", Value::Null, true).await;
        assert_eq!(status, StatusCode::OK);
        assert!(listing["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["daemon_id"] == id));
        assert!(!listing.to_string().contains(token));
        assert!(!listing.to_string().contains(&persisted));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app.clone()).await.unwrap();
        });
        let connect_request = |token: &str, daemon_id: &str| {
            let mut request = format!("ws://{address}/control")
                .into_client_request()
                .unwrap();
            request
                .headers_mut()
                .insert("authorization", format!("Bearer {token}").parse().unwrap());
            request
                .headers_mut()
                .insert("x-oneflow-daemon-id", daemon_id.parse().unwrap());
            request
        };
        let bad = tokio_tungstenite::connect_async(connect_request(token, "wrong-id"))
            .await
            .unwrap_err();
        assert!(
            matches!(bad, tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 401)
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(connect_request(token, &id))
            .await
            .unwrap();
        socket
            .send(Message::Text(
                json!({
                    "type":"Registration", "daemon_id":id, "cluster_name":"test",
                    "version":"test", "hostname":"test", "capabilities":["virtual-keys-v1"]
                })
                .to_string(),
            ))
            .await
            .unwrap();
        // Both initial messages must arrive before rotating the authenticated session.
        for _ in 0..2 {
            let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(message.is_text());
        }
        let mut second_state = state.clone();
        second_state.virtual_key_mode = Some(Arc::new(crate::VirtualKeyMode {
            store: second.clone(),
            admin_token: Arc::new("test-admin-token".into()),
        }));
        let app = crate::api::create_app(second_state, vec![]);
        let (status, rotated) = call(
            &app,
            "POST",
            &format!("/v1/daemons/{id}/rotate"),
            Value::Null,
            true,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let replacement = rotated["token"].as_str().unwrap();
        assert_ne!(replacement, token);
        assert!(!second.daemon_credential_valid(&identity).await.unwrap());
        let replacement_identity = AuthenticatedDaemon {
            daemon_id: id.clone(),
            token_hash: token_hash(replacement),
        };
        assert!(second
            .daemon_credential_valid(&replacement_identity)
            .await
            .unwrap());
        let closed = tokio::time::timeout(Duration::from_secs(7), socket.next())
            .await
            .unwrap();
        assert!(closed.is_none() || matches!(closed, Some(Ok(Message::Close(_)))));
        assert!(state.store.daemons().await.is_empty());
        let bad = tokio_tungstenite::connect_async(connect_request(token, &id))
            .await
            .unwrap_err();
        assert!(
            matches!(bad, tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 401)
        );

        let (mut socket, _) = tokio_tungstenite::connect_async(connect_request(replacement, &id))
            .await
            .unwrap();
        let (status, revoked) = call(
            &app,
            "POST",
            &format!("/v1/daemons/{id}/revoke"),
            Value::Null,
            true,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(revoked["revoked_at"].is_i64());
        assert!(!second
            .daemon_credential_valid(&replacement_identity)
            .await
            .unwrap());
        let closed = tokio::time::timeout(Duration::from_secs(7), socket.next())
            .await
            .unwrap();
        assert!(closed.is_none() || matches!(closed, Some(Ok(Message::Close(_)))));
        let (_, repeated) = call(
            &app,
            "POST",
            &format!("/v1/daemons/{id}/revoke"),
            Value::Null,
            true,
        )
        .await;
        assert_eq!(repeated, revoked);
        let restarted = VirtualKeyStore::connect(&url, &crate::store::default_config())
            .await
            .unwrap();
        assert!(!restarted
            .daemon_credential_valid(&replacement_identity)
            .await
            .unwrap());
        let (status, reactivated) = call(
            &app,
            "POST",
            &format!("/v1/daemons/{id}/rotate"),
            Value::Null,
            true,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(reactivated["credential"]["revoked_at"].is_null());
        assert!(restarted
            .daemon_credential_valid(&AuthenticatedDaemon {
                daemon_id: id.clone(),
                token_hash: token_hash(reactivated["token"].as_str().unwrap()),
            })
            .await
            .unwrap());
        for action in ["rotate", "revoke"] {
            assert_eq!(
                call(
                    &app,
                    "POST",
                    &format!("/v1/daemons/missing-{id}/{action}"),
                    Value::Null,
                    true
                )
                .await
                .0,
                StatusCode::NOT_FOUND
            );
        }
        store.revoke_daemon(&id, now_ts()).await.unwrap();
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }
}
