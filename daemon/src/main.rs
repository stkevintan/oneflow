mod api;
mod upstream;

use std::{
    collections::HashMap,
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use protocol::{ControlMessage, DaemonMessage, Heartbeat, ModelConfig, Registration, Telemetry};
use serde::Deserialize;
use tokio::{
    sync::{mpsc, RwLock},
    time,
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_READ_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(about = "Standalone LLM control-plane daemon")]
struct Args {
    /// Control-plane WebSocket endpoint, reachable from this daemon host.
    #[arg(long, value_name = "URL")]
    cp: String,
    /// Path to the daemon's TOML configuration file.
    #[arg(long, value_name = "PATH")]
    config: PathBuf,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DaemonConfig {
    listen_addr: String,
    daemon_id: String,
    connect_timeout_ms: u64,
    read_timeout_ms: u64,
    request_timeout_ms: u64,
    max_response_bytes: usize,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:0".to_string(),
            daemon_id: "daemon-poc-001".to_string(),
            connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
            read_timeout_ms: DEFAULT_READ_TIMEOUT_MS,
            request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }
}

impl DaemonConfig {
    fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read daemon config {}", path.display()))?;
        toml::from_str(&contents)
            .with_context(|| format!("failed to parse daemon config {}", path.display()))
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn example_config_has_an_ephemeral_listener() {
        let config: DaemonConfig = toml::from_str(include_str!("../config.example.toml")).unwrap();
        let listen_addr: SocketAddr = config.listen_addr.parse().unwrap();

        assert_eq!(listen_addr.port(), 0);
    }

    #[test]
    fn cli_requires_control_plane_and_config_arguments() {
        assert!(Args::try_parse_from(["daemon", "--cp", "ws://localhost:4000/control"]).is_err());
        assert!(Args::try_parse_from(["daemon", "--config", "daemon.toml"]).is_err());
        assert!(Args::try_parse_from([
            "daemon",
            "--cp",
            "ws://localhost:4000/control",
            "--config",
            "daemon.toml"
        ])
        .is_ok());
    }
}

/// Tunables resolved once at startup from the config file and environment.
#[derive(Clone)]
pub(crate) struct DaemonSettings {
    pub(crate) daemon_id: String,
    /// Client-facing keys for `/v1/*`. Empty means the API is open.
    pub(crate) api_keys: Vec<String>,
    pub(crate) connect_timeout: Duration,
    pub(crate) read_timeout: Duration,
    pub(crate) request_timeout: Duration,
    pub(crate) max_response_bytes: usize,
}

impl DaemonSettings {
    fn from_config(config: &DaemonConfig) -> Self {
        Self {
            daemon_id: env::var("DAEMON_ID").unwrap_or_else(|_| config.daemon_id.clone()),
            api_keys: env_list("DAEMON_API_KEYS"),
            connect_timeout: env_duration("UPSTREAM_CONNECT_TIMEOUT_MS", config.connect_timeout_ms),
            read_timeout: env_duration("UPSTREAM_READ_TIMEOUT_MS", config.read_timeout_ms),
            request_timeout: env_duration("UPSTREAM_REQUEST_TIMEOUT_MS", config.request_timeout_ms),
            max_response_bytes: env_usize("MAX_UPSTREAM_RESPONSE_BYTES", config.max_response_bytes),
        }
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) settings: Arc<DaemonSettings>,
    pub(crate) models: Arc<RwLock<HashMap<String, ModelConfig>>>,
    pub(crate) models_updated_at: Arc<AtomicI64>,
    pub(crate) active_requests: Arc<AtomicU32>,
    pub(crate) dropped_telemetry: Arc<AtomicU64>,
    pub(crate) telemetry_tx: mpsc::Sender<DaemonMessage>,
    pub(crate) client: reqwest::Client,
    pub(crate) started_at: i64,
}

impl AppState {
    pub(crate) fn new(
        settings: DaemonSettings,
        telemetry_tx: mpsc::Sender<DaemonMessage>,
    ) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .connect_timeout(settings.connect_timeout)
            .read_timeout(settings.read_timeout)
            .build()?;

        Ok(Self {
            settings: Arc::new(settings),
            models: Arc::new(RwLock::new(HashMap::new())),
            models_updated_at: Arc::new(AtomicI64::new(now_ts())),
            active_requests: Arc::new(AtomicU32::new(0)),
            dropped_telemetry: Arc::new(AtomicU64::new(0)),
            telemetry_tx,
            client,
            started_at: now_ts(),
        })
    }

    pub(crate) async fn set_models(&self, models: Vec<ModelConfig>) {
        let mut guard = self.models.write().await;
        *guard = models
            .into_iter()
            .map(|model| (model.model_name.clone(), model))
            .collect();
        drop(guard);
        self.models_updated_at.store(now_ts(), Ordering::Relaxed);
    }

    pub(crate) async fn model(&self, name: &str) -> Option<ModelConfig> {
        self.models.read().await.get(name).cloned()
    }

    pub(crate) async fn model_count(&self) -> usize {
        self.models.read().await.len()
    }

    pub(crate) async fn model_names(&self) -> Vec<String> {
        self.models.read().await.keys().cloned().collect()
    }

    /// Non-blocking telemetry emit. Drops are counted rather than logged per
    /// event, and surfaced through `/healthz`.
    pub(crate) fn report(&self, event: Telemetry) {
        if self
            .telemetry_tx
            .try_send(DaemonMessage::Telemetry(event))
            .is_err()
        {
            self.dropped_telemetry.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Keeps the in-flight request counter accurate for as long as the request — or
/// the streaming response body — is alive.
pub(crate) struct ActiveRequestGuard(Arc<AtomicU32>);

impl ActiveRequestGuard {
    pub(crate) fn new(counter: Arc<AtomicU32>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(counter)
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(crate) fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn env_duration(name: &str, default_ms: u64) -> Duration {
    match env::var(name) {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(ms) => Duration::from_millis(ms),
            Err(err) => {
                warn!(%name, %value, error = %err, "invalid duration, using default");
                Duration::from_millis(default_ms)
            }
        },
        Err(_) => Duration::from_millis(default_ms),
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    match env::var(name) {
        Ok(value) => match value.trim().parse::<usize>() {
            Ok(parsed) => parsed,
            Err(err) => {
                warn!(%name, %value, error = %err, "invalid number, using default");
                default
            }
        },
        Err(_) => default,
    }
}

fn env_list(name: &str) -> Vec<String> {
    env::var(name)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "daemon=info".into()),
        )
        .init();

    let args = Args::parse();
    let config = DaemonConfig::load(&args.config)?;
    let settings = DaemonSettings::from_config(&config);
    let control_plane_url = args.cp;
    let listen_addr: SocketAddr = config
        .listen_addr
        .parse()
        .with_context(|| format!("invalid daemon listen_addr: {}", config.listen_addr))?;
    let cluster_name = env::var("CLUSTER_NAME").unwrap_or_else(|_| "cluster-ssh198".to_string());
    let hostname = env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string());

    if settings.api_keys.is_empty() {
        warn!("DAEMON_API_KEYS is unset: /v1/* is open to anyone who can reach this port");
    }

    let (telemetry_tx, telemetry_rx) = mpsc::channel(256);
    let state = AppState::new(settings, telemetry_tx)?;
    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    let bound_addr = listener.local_addr()?;

    tokio::spawn(run_control_channel(
        control_plane_url,
        cluster_name,
        hostname,
        state.clone(),
        telemetry_rx,
    ));
    let app = api::create_app(state);
    info!(%bound_addr, "daemon OpenAI-compatible API is ready");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn run_control_channel(
    control_plane_url: String,
    cluster_name: String,
    hostname: String,
    state: AppState,
    mut telemetry_rx: mpsc::Receiver<DaemonMessage>,
) {
    loop {
        match connect_async(&control_plane_url).await {
            Ok((mut socket, _)) => {
                info!(url = %control_plane_url, "connected to control plane");
                let registration = DaemonMessage::Registration(Registration {
                    daemon_id: state.settings.daemon_id.clone(),
                    cluster_name: cluster_name.clone(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    capabilities: vec!["openai-compatible".to_string()],
                    hostname: hostname.clone(),
                });

                if send_daemon_message(&mut socket, &registration)
                    .await
                    .is_ok()
                {
                    let mut heartbeat_interval = time::interval(Duration::from_secs(5));
                    heartbeat_interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

                    loop {
                        tokio::select! {
                            _ = heartbeat_interval.tick() => {
                                let heartbeat = DaemonMessage::Heartbeat(Heartbeat {
                                    daemon_id: state.settings.daemon_id.clone(),
                                    active_requests: state.active_requests.load(Ordering::Relaxed),
                                    loaded_models: state.model_names().await,
                                });
                                if send_daemon_message(&mut socket, &heartbeat).await.is_err() {
                                    break;
                                }
                            }
                            Some(telemetry) = telemetry_rx.recv() => {
                                if send_daemon_message(&mut socket, &telemetry).await.is_err() {
                                    break;
                                }
                            }
                            incoming = socket.next() => {
                                match incoming {
                                    Some(Ok(Message::Text(text))) => {
                                        handle_control_message(&text, &state).await;
                                    }
                                    Some(Ok(Message::Ping(payload))) => {
                                        if socket.send(Message::Pong(payload)).await.is_err() {
                                            break;
                                        }
                                    }
                                    Some(Ok(Message::Close(_))) | None => break,
                                    Some(Err(err)) => {
                                        warn!(error = %err, "control channel error");
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                warn!("control plane connection closed");
            }
            Err(err) => {
                warn!(url = %control_plane_url, error = %err, "control plane connection failed")
            }
        }

        time::sleep(Duration::from_secs(5)).await;
    }
}

async fn handle_control_message(text: &str, state: &AppState) {
    match serde_json::from_str::<ControlMessage>(text) {
        Ok(ControlMessage::ConfigUpdate { models, version }) => {
            let model_count = models.len();
            state.set_models(models).await;
            info!(%version, model_count, "applied model configuration");
        }
        Ok(ControlMessage::Command { cmd, reason }) => {
            info!(%cmd, %reason, "received control command");
        }
        Err(err) => warn!(error = %err, "received invalid control message"),
    }
}

async fn send_daemon_message<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    message: &DaemonMessage,
) -> Result<(), ()>
where
    tokio_tungstenite::WebSocketStream<S>:
        futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let json = serde_json::to_string(message).map_err(|err| {
        error!(error = %err, "failed to serialize daemon message");
    })?;
    socket
        .send(Message::Text(json.into()))
        .await
        .map_err(|err| {
            warn!(error = %err, "failed to send control channel message");
        })
}
