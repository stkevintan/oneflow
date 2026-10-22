mod api;
mod control;
mod key_api;
mod store;
mod virtual_keys;

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use axum::http::HeaderValue;
use protocol::ControlMessage;
use serde::{
    de::{MapAccess, Visitor},
    Deserialize,
};
use subtle::ConstantTimeEq;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use store::{default_config, MemoryStore};
use virtual_keys::VirtualKeyStore;

#[derive(Clone)]
pub(crate) struct VirtualKeyMode {
    pub(crate) store: Arc<VirtualKeyStore>,
    pub(crate) admin_token: Arc<String>,
    pub(crate) daemon_tokens: Arc<HashMap<String, String>>,
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) store: Arc<MemoryStore>,
    pub(crate) broadcast: broadcast::Sender<ControlMessage>,
    pub(crate) started_at: i64,
    pub(crate) virtual_key_mode: Option<Arc<VirtualKeyMode>>,
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

struct StrictDaemonTokens(HashMap<String, String>);

impl<'de> Deserialize<'de> for StrictDaemonTokens {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TokenMapVisitor;

        impl<'de> Visitor<'de> for TokenMapVisitor {
            type Value = StrictDaemonTokens;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object with unique daemon IDs and string tokens")
            }

            fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut tokens = HashMap::new();
                while let Some((daemon_id, token)) = map.next_entry::<String, String>()? {
                    if tokens.insert(daemon_id, token).is_some() {
                        return Err(serde::de::Error::custom("duplicate daemon ID"));
                    }
                }
                Ok(StrictDaemonTokens(tokens))
            }
        }

        deserializer.deserialize_map(TokenMapVisitor)
    }
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
    let virtual_key_mode = load_virtual_key_mode(&fallback_config).await?;
    let initial_config = match &virtual_key_mode {
        Some(mode) => mode.store.config().await?,
        None => fallback_config,
    };
    let state = AppState {
        store: Arc::new(MemoryStore::new(initial_config)),
        broadcast,
        started_at: now_ts(),
        virtual_key_mode: virtual_key_mode.clone(),
    };

    if let Some(mode) = virtual_key_mode {
        spawn_database_notifications(state.clone(), mode.store.clone());
    }

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

async fn load_virtual_key_mode(
    initial_config: &protocol::ModelConfigSet,
) -> Result<Option<Arc<VirtualKeyMode>>> {
    let database_url = optional_env("ONEFLOW_DATABASE_URL")?;
    let admin_token = optional_env("ONEFLOW_ADMIN_TOKEN")?;
    let daemon_tokens = optional_env("ONEFLOW_DAEMON_TOKENS")?;
    let configured = [
        database_url.is_some(),
        admin_token.is_some(),
        daemon_tokens.is_some(),
    ];
    if configured.iter().all(|value| !value) {
        return Ok(None);
    }
    if configured.iter().any(|value| !value) {
        anyhow::bail!(
            "virtual-key mode requires ONEFLOW_DATABASE_URL, ONEFLOW_ADMIN_TOKEN, and ONEFLOW_DAEMON_TOKENS together"
        );
    }

    let admin_token = admin_token.expect("the setting was validated above");
    validate_bearer_secret("ONEFLOW_ADMIN_TOKEN", &admin_token)?;
    let daemon_tokens = serde_json::from_str::<StrictDaemonTokens>(
        &daemon_tokens.expect("the setting was validated above"),
    )
    .context(
        "ONEFLOW_DAEMON_TOKENS must be a JSON object with unique daemon IDs and string tokens",
    )?
    .0;
    if daemon_tokens.is_empty() {
        anyhow::bail!("ONEFLOW_DAEMON_TOKENS must contain at least one daemon credential");
    }
    let mut unique_daemon_tokens = HashSet::new();
    for (daemon_id, token) in &daemon_tokens {
        if daemon_id.is_empty()
            || daemon_id.len() > 128
            || !daemon_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            anyhow::bail!("ONEFLOW_DAEMON_TOKENS contains an invalid daemon ID");
        }
        validate_bearer_secret("ONEFLOW_DAEMON_TOKENS entry", token)?;
        if !unique_daemon_tokens.insert(token) {
            anyhow::bail!("ONEFLOW_DAEMON_TOKENS must use a distinct token for each daemon ID");
        }
        if constant_time_token_eq(&admin_token, token) {
            anyhow::bail!("administrator and daemon credentials must be distinct");
        }
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

    let key_store = Arc::new(
        VirtualKeyStore::connect(
            &database_url.expect("the setting was validated above"),
            initial_config,
        )
        .await?,
    );
    Ok(Some(Arc::new(VirtualKeyMode {
        store: key_store,
        admin_token: Arc::new(admin_token),
        daemon_tokens: Arc::new(daemon_tokens),
    })))
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
