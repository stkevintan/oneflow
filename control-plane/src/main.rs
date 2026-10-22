mod api;
mod control;
mod daemon_credentials;
mod key_api;
mod store;
mod virtual_keys;

use std::{
    net::SocketAddr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use axum::http::HeaderValue;
use protocol::ControlMessage;
use subtle::ConstantTimeEq;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use store::{default_config, MemoryStore};
use virtual_keys::VirtualKeyStore;

#[derive(Clone)]
pub(crate) struct AccessControl {
    pub(crate) store: Arc<VirtualKeyStore>,
    pub(crate) admin_token: Arc<String>,
    pub(crate) control_plane_url: String,
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) store: Arc<MemoryStore>,
    pub(crate) broadcast: broadcast::Sender<ControlMessage>,
    pub(crate) started_at: i64,
    pub(crate) access: Arc<AccessControl>,
}

pub(crate) fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

pub(crate) fn constant_time_token_eq(expected: &str, supplied: &str) -> bool {
    expected.as_bytes().ct_eq(supplied.as_bytes()).into()
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let config = config_provider::ConfigProvider::load()?;
    let addr: SocketAddr = config.control_plane.bind_addr.parse().with_context(|| {
        format!(
            "invalid control_plane.bind_addr: {}",
            config.control_plane.bind_addr
        )
    })?;
    let cors_allowed_origins = config
        .control_plane
        .cors_allowed_origins
        .iter()
        .map(|origin| {
            HeaderValue::from_str(origin).with_context(|| {
                format!("invalid control_plane.cors_allowed_origins entry: {origin}")
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let (broadcast, _) = broadcast::channel(32);
    let fallback_config = default_config();
    let access = load_access_control(&config.control_plane, &fallback_config).await?;
    let initial_config = access.store.config().await?;
    let state = AppState {
        store: Arc::new(MemoryStore::new(initial_config)),
        broadcast,
        started_at: now_ts(),
        access: access.clone(),
    };

    spawn_database_notifications(state.clone(), access.store.clone());

    let app = api::create_app(state, cors_allowed_origins);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("🚀 Control Plane API listening on http://{}", addr);
    info!(
        "   Management API:     http://{}/v1 (contract/openapi.yaml)",
        addr
    );
    info!("   WebSocket endpoint: ws://{}/control", addr);

    axum::serve(listener, app).await?;
    Ok(())
}

async fn load_access_control(
    config: &config_provider::ControlPlaneConfig,
    initial_config: &protocol::ModelConfigSet,
) -> Result<Arc<AccessControl>> {
    validate_control_plane_url(&config.websocket_url)?;
    if std::env::var_os("ONEFLOW_DAEMON_TOKENS").is_some() {
        anyhow::bail!("ONEFLOW_DAEMON_TOKENS is no longer supported; provision daemon credentials through POST /v1/daemons");
    }
    let (database_url, admin_token) = resolve_virtual_key_settings(
        config,
        optional_env("ONEFLOW_DATABASE_URL")?,
        optional_env("ONEFLOW_ADMIN_TOKEN")?,
    )?;
    let key_store = Arc::new(VirtualKeyStore::connect(&database_url, initial_config).await?);
    Ok(Arc::new(AccessControl {
        store: key_store,
        admin_token: Arc::new(admin_token),
        control_plane_url: config.websocket_url.clone(),
    }))
}

fn validate_control_plane_url(value: &str) -> Result<()> {
    let url = url::Url::parse(value).context("invalid control_plane.websocket_url")?;
    anyhow::ensure!(
        url.host_str().is_some()
            && matches!(url.scheme(), "ws" | "wss")
            && url.username().is_empty() && url.password().is_none()
            && url.query().is_none() && url.fragment().is_none(),
        "control_plane.websocket_url must be a WS or WSS URL with a host and without credentials, query, or fragment"
    );
    Ok(())
}

type VirtualKeySettings = (String, String);

fn resolve_virtual_key_settings(
    config: &config_provider::ControlPlaneConfig,
    database_url: Option<String>,
    admin_token: Option<String>,
) -> Result<VirtualKeySettings> {
    let database_url = database_url.or_else(|| config.database_url.clone());
    let admin_token = admin_token.or_else(|| config.admin_token.clone());
    let configured = [database_url.is_some(), admin_token.is_some()];
    if configured.iter().any(|value| !value) {
        anyhow::bail!(
            "control_plane.database_url and admin_token are required (or ONEFLOW_DATABASE_URL and ONEFLOW_ADMIN_TOKEN overrides)"
        );
    }

    let admin_token = admin_token.expect("the setting was validated above");
    validate_bearer_secret("administrator token", &admin_token)?;
    if database_url
        .as_ref()
        .is_some_and(|url| url.trim().is_empty())
    {
        anyhow::bail!("database_url must not be empty");
    }
    Ok((
        database_url.expect("the setting was validated above"),
        admin_token,
    ))
}

fn optional_env(name: &str) -> Result<Option<String>> {
    std::env::var_os(name)
        .map(|value| {
            value
                .into_string()
                .map_err(|_| anyhow::anyhow!("{name} must contain valid UTF-8"))
        })
        .transpose()
}

fn validate_bearer_secret(variable: &str, secret: &str) -> Result<()> {
    if secret.len() < 32
        || secret.trim() != secret
        || secret.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        anyhow::bail!("{variable} entries must be at least 32 non-whitespace bytes");
    }
    Ok(())
}

fn spawn_database_notifications(state: AppState, key_store: Arc<VirtualKeyStore>) {
    tokio::spawn(async move {
        loop {
            match key_store.listener().await {
                Ok(mut listener) => {
                    reload_model_config(&state, &key_store, true).await;
                    refresh_policy_snapshot(&state, &key_store).await;
                    let mut refresh = tokio::time::interval_at(
                        tokio::time::Instant::now() + std::time::Duration::from_secs(60),
                        std::time::Duration::from_secs(60),
                    );
                    loop {
                        tokio::select! {
                            notification = listener.recv() => match notification {
                                Ok(notification) if notification.channel() == "oneflow_model_config" => {
                                    reload_model_config(&state, &key_store, true).await;
                                }
                                Ok(notification) if notification.channel() == "oneflow_virtual_key_policy" => {
                                    refresh_policy_snapshot(&state, &key_store).await;
                                }
                                Ok(_) => {}
                                Err(err) => {
                                    warn!(error = %err, "virtual-key database notification listener disconnected");
                                    break;
                                }
                            },
                            _ = refresh.tick() => {
                                reload_model_config(&state, &key_store, false).await;
                            }
                        }
                    }
                }
                Err(err) => {
                    warn!(error = %err, "failed to connect virtual-key notification listener")
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
}

async fn reload_model_config(state: &AppState, key_store: &VirtualKeyStore, force: bool) {
    match key_store.config().await {
        Ok(config) => {
            let changed = serde_json::to_value(state.store.config().await).ok()
                != serde_json::to_value(&config).ok();
            if changed {
                state.store.set_config(config.clone()).await;
            }
            if force || changed {
                let _ = state.broadcast.send(ControlMessage::from(config));
            }
        }
        Err(err) => error!(error = %err, "failed to reload durable model configuration"),
    }
}

async fn refresh_policy_snapshot(state: &AppState, key_store: &VirtualKeyStore) {
    match key_store.snapshot(now_ts()).await {
        Ok(snapshot) => {
            let _ = state
                .broadcast
                .send(ControlMessage::VirtualKeyPolicySnapshot(snapshot));
        }
        Err(err) => error!(error = %err, "failed to refresh virtual-key policy snapshot"),
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;

    #[test]
    fn provisioning_url_accepts_ws_and_wss_without_credentials() {
        for value in [
            "wss://control.example/control",
            "ws://127.0.0.1:4000/control",
            "ws://[::1]/control",
            "ws://remote.example/control",
            "ws://192.168.2.148:4000/control",
        ] {
            assert!(validate_control_plane_url(value).is_ok(), "{value}");
        }
        for value in [
            "",
            "https://control.example/control",
            "wss://user:password@control.example/control",
            "wss://control.example/control?token=secret",
            "wss://control.example/control#fragment",
        ] {
            assert!(validate_control_plane_url(value).is_err(), "{value}");
        }
    }

    fn config() -> config_provider::ControlPlaneConfig {
        config_provider::ControlPlaneConfig {
            bind_addr: "127.0.0.1:4000".into(),
            websocket_url: "ws://127.0.0.1:4000/control".into(),
            cors_allowed_origins: vec![],
            database_url: None,
            admin_token: None,
        }
    }

    fn configured() -> config_provider::ControlPlaneConfig {
        config_provider::ControlPlaneConfig {
            database_url: Some("postgres://localhost/oneflow".into()),
            admin_token: Some("a".repeat(32)),
            ..config()
        }
    }

    #[test]
    fn missing_and_partial_configuration_is_rejected() {
        assert!(resolve_virtual_key_settings(&config(), None, None).is_err());
        assert!(resolve_virtual_key_settings(
            &config(),
            Some("postgres://localhost/app".into()),
            None
        )
        .is_err());
        let mut partial = configured();
        partial.admin_token = None;
        assert!(resolve_virtual_key_settings(&partial, None, None).is_err());
    }

    #[test]
    fn toml_settings_and_per_setting_environment_overrides() {
        let config = configured();
        let (url, admin) = resolve_virtual_key_settings(&config, None, None).unwrap();
        assert_eq!(url, config.database_url.clone().unwrap());
        assert_eq!(admin, "a".repeat(32));
        let (url, admin) = resolve_virtual_key_settings(
            &config,
            Some("postgres://localhost/override".into()),
            Some("b".repeat(32)),
        )
        .unwrap();
        assert_eq!(url, "postgres://localhost/override");
        assert_eq!(admin, "b".repeat(32));
        let (_, admin) = resolve_virtual_key_settings(&config, None, Some("b".repeat(32))).unwrap();
        assert_eq!(admin, "b".repeat(32));
    }

    #[test]
    fn rejects_invalid_credentials_and_empty_overrides() {
        assert!(resolve_virtual_key_settings(&configured(), Some(String::new()), None).is_err());
        assert!(resolve_virtual_key_settings(&configured(), None, Some(String::new())).is_err());
    }
}
