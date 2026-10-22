mod api;
mod auth;
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
use auth::PolicyCache;
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use protocol::{ControlMessage, DaemonMessage, Heartbeat, ModelConfig, Registration, Telemetry};
use serde::Deserialize;
use tokio::{
    sync::{mpsc, RwLock},
    time,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
};
use tracing::{error, info, warn};

const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_READ_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:9000";

#[derive(Debug, Parser)]
#[command(about = "Oneflow standalone inference daemon")]
struct Args {
    /// Control-plane WebSocket endpoint, reachable from this daemon host.
    #[arg(long, value_name = "URL")]
    cp: Option<String>,
    /// Config file to read (defaults to ./config.toml when present).
    #[arg(short, long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// HTTP listener address.
    #[arg(long, value_name = "SOCKET")]
    listen_addr: Option<String>,
    /// Daemon identity reported during registration.
    #[arg(long)]
    daemon_id: Option<String>,
    /// Upstream TCP/TLS connect timeout in milliseconds.
    #[arg(long)]
    connect_timeout_ms: Option<u64>,
    /// Upstream per-read timeout in milliseconds.
    #[arg(long)]
    read_timeout_ms: Option<u64>,
    /// Upstream request timeout in milliseconds.
    #[arg(long)]
    request_timeout_ms: Option<u64>,
    /// Maximum buffered upstream response size in bytes.
    #[arg(long)]
    max_response_bytes: Option<usize>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DaemonConfig {
    control_token: Option<String>,
    cp: Option<String>,
    listen_addr: Option<String>,
    daemon_id: Option<String>,
    connect_timeout_ms: Option<u64>,
    read_timeout_ms: Option<u64>,
    request_timeout_ms: Option<u64>,
    max_response_bytes: Option<usize>,
}

impl DaemonConfig {
    fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read daemon config {}", path.display()))?;
        Self::load_from_str(&contents)
            .with_context(|| format!("failed to parse daemon config {}", path.display()))
    }

    fn load_from_str(contents: &str) -> Result<Self> {
        toml::from_str(contents).map_err(|err: toml::de::Error| {
            anyhow::anyhow!("invalid daemon configuration: {}", err.message())
        })
    }

    fn load_default_or_explicit(path: Option<&Path>) -> Result<Self> {
        if let Some(path) = path {
            return Self::load(path);
        }

        let path = Path::new("config.toml");
        if path.is_file() {
            Self::load(path)
        } else {
            Ok(Self::default())
        }
    }

    fn resolve(args: &Args, file: Self) -> Result<ResolvedConfig> {
        let token = env::var_os("ONEFLOW_DAEMON_TOKEN")
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("ONEFLOW_DAEMON_TOKEN must be UTF-8"))
            })
            .transpose()?;
        Self::resolve_with_token(args, file, token)
    }

    fn resolve_with_token(
        args: &Args,
        file: Self,
        control_token: Option<String>,
    ) -> Result<ResolvedConfig> {
        let control_token = control_token.or(file.control_token.clone()).context(
            "daemon token required: provide ONEFLOW_DAEMON_TOKEN or the TOML control_token setting",
        )?;
        let cp = args
            .cp
            .clone()
            .or_else(|| env::var("CONTROL_PLANE_URL").ok())
            .or(file.cp)
            .context("control-plane URL required: provide --cp, CONTROL_PLANE_URL, or the TOML cp setting")?;

        let listen_addr = args
            .listen_addr
            .clone()
            .or_else(|| env::var("DAEMON_LISTEN_ADDR").ok())
            .or(file.listen_addr)
            .unwrap_or_else(|| DEFAULT_LISTEN_ADDR.to_string());
        let daemon_id = args
            .daemon_id
            .clone()
            .or_else(|| env::var("DAEMON_ID").ok())
            .or(file.daemon_id)
            .unwrap_or_else(|| "daemon-poc-001".to_string());
        let connect_timeout_ms = args
            .connect_timeout_ms
            .or_else(|| env_u64("UPSTREAM_CONNECT_TIMEOUT_MS"))
            .or(file.connect_timeout_ms)
            .unwrap_or(DEFAULT_CONNECT_TIMEOUT_MS);
        let read_timeout_ms = args
            .read_timeout_ms
            .or_else(|| env_u64("UPSTREAM_READ_TIMEOUT_MS"))
            .or(file.read_timeout_ms)
            .unwrap_or(DEFAULT_READ_TIMEOUT_MS);
        let request_timeout_ms = args
            .request_timeout_ms
            .or_else(|| env_u64("UPSTREAM_REQUEST_TIMEOUT_MS"))
            .or(file.request_timeout_ms)
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
        let max_response_bytes = args
            .max_response_bytes
            .or_else(|| env_usize("MAX_UPSTREAM_RESPONSE_BYTES"))
            .or(file.max_response_bytes)
            .unwrap_or(DEFAULT_MAX_RESPONSE_BYTES);

        anyhow::ensure!(
            control_token.len() >= 32
                && !control_token.bytes().any(|byte| byte.is_ascii_whitespace()),
            "ONEFLOW_DAEMON_TOKEN must be at least 32 non-whitespace bytes"
        );
        let url = url::Url::parse(&cp).context("invalid control-plane URL")?;
        anyhow::ensure!(
            url.host_str().is_some() && matches!(url.scheme(), "ws" | "wss"),
            "control-plane URL must use ws or wss and include a host"
        );
        anyhow::ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "put control credentials in ONEFLOW_DAEMON_TOKEN, not the URL"
        );
        Ok(ResolvedConfig {
            control_token,
            cp,
            listen_addr,
            daemon_id,
            connect_timeout_ms,
            read_timeout_ms,
            request_timeout_ms,
            max_response_bytes,
        })
    }
}

struct ResolvedConfig {
    control_token: String,
    cp: String,
    listen_addr: String,
    daemon_id: String,
    connect_timeout_ms: u64,
    read_timeout_ms: u64,
    request_timeout_ms: u64,
    max_response_bytes: usize,
}

#[cfg(test)]
mod config_tests {
    use super::*;

    fn resolve(args: &Args, file: DaemonConfig) -> Result<ResolvedConfig> {
        DaemonConfig::resolve_with_token(args, file, Some("t".repeat(64)))
    }

    #[test]
    fn example_config_has_a_fixed_local_listener() {
        let config = DaemonConfig::load_from_str(include_str!("../config.example.toml")).unwrap();
        let listen_addr: SocketAddr = config.listen_addr.unwrap().parse().unwrap();

        assert!(listen_addr.ip().is_loopback());
        assert_eq!(listen_addr.port(), 9000);
    }

    #[test]
    fn defaults_to_a_fixed_local_listener() {
        let args =
            Args::try_parse_from(["oneflowd", "--cp", "ws://localhost:4000/control"]).unwrap();
        let resolved = resolve(&args, DaemonConfig::default()).unwrap();
        let listen_addr: SocketAddr = resolved.listen_addr.parse().unwrap();

        assert!(listen_addr.ip().is_loopback());
        assert_eq!(listen_addr.port(), 9000);
    }

    #[test]
    fn cli_allows_config_file_or_command_line_values() {
        assert!(Args::try_parse_from(["daemon"]).is_ok());
        assert!(Args::try_parse_from(["daemon", "--cp", "ws://localhost:4000/control"]).is_ok());
        assert!(Args::try_parse_from(["daemon", "-c", "daemon.toml"]).is_ok());
        assert!(Args::try_parse_from([
            "daemon",
            "--cp",
            "ws://localhost:4000/control",
            "--config",
            "daemon.toml"
        ])
        .is_ok());
    }

    #[test]
    fn credentials_and_valid_control_url_are_required() {
        let args = Args::try_parse_from(["oneflowd", "--cp", "ws://localhost/control"]).unwrap();
        for token in [
            "",
            "short",
            "contains whitespace in a token of sufficient length",
        ] {
            assert!(DaemonConfig::resolve_with_token(
                &args,
                DaemonConfig::default(),
                Some(token.into())
            )
            .is_err());
        }
        for url in [
            "https://remote.example/control",
            "wss://user:password@remote.example/control",
            "wss://remote.example/control?token=secret",
            "wss://remote.example/control#fragment",
        ] {
            let args = Args::try_parse_from(["oneflowd", "--cp", url]).unwrap();
            assert!(resolve(&args, DaemonConfig::default()).is_err(), "{url}");
        }
        for url in [
            "wss://remote.example/control",
            "ws://127.0.0.1:4000/control",
            "ws://[::1]:4000/control",
            "ws://remote.example/control",
            "ws://192.168.2.148:4000/control",
        ] {
            let args = Args::try_parse_from(["oneflowd", "--cp", url]).unwrap();
            assert!(resolve(&args, DaemonConfig::default()).is_ok(), "{url}");
        }
    }

    #[test]
    fn removed_auth_mode_options_are_rejected() {
        assert!(Args::try_parse_from(["oneflowd", "--auth-mode", "legacy"]).is_err());
        assert!(DaemonConfig::load_from_str("auth_mode = \"legacy\"").is_err());
        assert!(DaemonConfig::load_from_str("auth_mode = \"virtual_keys\"").is_err());
    }

    #[test]
    fn downloaded_config_is_standalone_and_environment_token_overrides_file() {
        let token = "ofd_".to_string() + &"a".repeat(64);
        let contents = format!(
            "cp = \"wss://control.example/control\"\ndaemon_id = \"server-a\"\ncontrol_token = \"{token}\"\nlisten_addr = \"127.0.0.1:9000\"\n"
        );
        let file = DaemonConfig::load_from_str(&contents).unwrap();
        let args = Args::try_parse_from(["oneflowd", "-c", "config.toml"]).unwrap();
        let resolved = DaemonConfig::resolve_with_token(&args, file.clone(), None).unwrap();
        assert_eq!(resolved.cp, "wss://control.example/control");
        assert_eq!(resolved.daemon_id, "server-a");
        assert_eq!(resolved.control_token, token);
        let overridden =
            DaemonConfig::resolve_with_token(&args, file.clone(), Some("b".repeat(64))).unwrap();
        assert_eq!(overridden.control_token, "b".repeat(64));
        assert!(DaemonConfig::resolve_with_token(&args, file, Some(String::new())).is_err());
        let empty = DaemonConfig::default();
        assert!(DaemonConfig::resolve_with_token(&args, empty, None).is_err());
    }

    #[test]
    fn config_parse_errors_do_not_include_secret_source() {
        let secret = "private-daemon-credential";
        let error = DaemonConfig::load_from_str(&format!("control_token = \"{secret}"))
            .err()
            .unwrap();
        assert!(!format!("{error:#}").contains(secret));
    }

    #[test]
    fn config_file_reads_standalone_daemon_settings() {
        let config = DaemonConfig::load_from_str(
            r#"
                cp = "ws://daemon.example:4000/control"
                listen_addr = "127.0.0.1:9000"
            "#,
        )
        .unwrap();

        assert_eq!(
            config.cp.as_deref(),
            Some("ws://daemon.example:4000/control")
        );
        assert_eq!(config.listen_addr.as_deref(), Some("127.0.0.1:9000"));
    }

    #[test]
    fn cli_values_override_file_values() {
        let args = Args::try_parse_from([
            "daemon",
            "--cp",
            "wss://cli.example:4000/control",
            "--listen-addr",
            "127.0.0.1:9001",
        ])
        .unwrap();
        let file = DaemonConfig {
            cp: Some("ws://file.example:4000/control".to_string()),
            listen_addr: Some("127.0.0.1:9000".to_string()),
            ..DaemonConfig::default()
        };

        let resolved = resolve(&args, file).unwrap();
        assert_eq!(resolved.cp, "wss://cli.example:4000/control");
        assert_eq!(resolved.listen_addr, "127.0.0.1:9001");
    }

    #[tokio::test]
    async fn authenticated_control_channel_applies_snapshot_and_acknowledges() {
        let args = Args::try_parse_from(["oneflowd", "--cp", "ws://localhost/control"]).unwrap();
        let resolved = resolve(&args, DaemonConfig::default()).unwrap();
        let mut settings = DaemonSettings::from_config(&resolved);
        settings.control_token = "test-control-token".into();
        let request = control_request("ws://localhost/control", &settings).unwrap();
        assert_eq!(
            request.headers()["authorization"],
            "Bearer test-control-token"
        );
        assert_eq!(request.headers()["x-oneflow-daemon-id"], settings.daemon_id);
        let (tx, rx) = mpsc::channel(1);
        let state = AppState::new(settings, tx).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let message = socket.next().await.unwrap().unwrap();
            let registration: DaemonMessage =
                serde_json::from_str(message.to_text().unwrap()).unwrap();
            match registration {
                DaemonMessage::Registration(reg) => {
                    assert!(reg.capabilities.iter().any(|cap| cap == "virtual-keys-v1"))
                }
                _ => panic!("first frame must register"),
            }
            let snapshot = protocol::VirtualKeyPolicySnapshot {
                protocol_version: 1,
                revision: 7,
                issued_at: now_ts(),
                valid_until: now_ts() + 300,
                keys: vec![],
            };
            let deadline = snapshot.valid_until;
            socket
                .send(Message::Text(
                    serde_json::to_string(&ControlMessage::VirtualKeyPolicySnapshot(snapshot))
                        .unwrap(),
                ))
                .await
                .unwrap();
            loop {
                let message = socket.next().await.unwrap().unwrap();
                let message: DaemonMessage =
                    serde_json::from_str(message.to_text().unwrap()).unwrap();
                if let DaemonMessage::VirtualKeyPolicyAck(ack) = message {
                    assert_eq!(ack.revision, 7);
                    assert_eq!(ack.valid_until, deadline);
                    break;
                }
            }
        });
        let task = tokio::spawn(run_control_channel(
            format!("ws://{address}/control"),
            "test".into(),
            "test".into(),
            state,
            rx,
        ));
        let result = time::timeout(Duration::from_secs(3), server).await;
        task.abort();
        let _ = task.await;
        result
            .expect("policy acknowledgement must not deadlock")
            .unwrap();
    }
}

/// Tunables resolved once at startup from the config file and environment.
#[derive(Clone)]
pub(crate) struct DaemonSettings {
    pub(crate) control_token: String,
    pub(crate) daemon_id: String,
    pub(crate) connect_timeout: Duration,
    pub(crate) read_timeout: Duration,
    pub(crate) request_timeout: Duration,
    pub(crate) max_response_bytes: usize,
}

impl DaemonSettings {
    fn from_config(config: &ResolvedConfig) -> Self {
        Self {
            control_token: config.control_token.clone(),
            daemon_id: config.daemon_id.clone(),
            connect_timeout: Duration::from_millis(config.connect_timeout_ms),
            read_timeout: Duration::from_millis(config.read_timeout_ms),
            request_timeout: Duration::from_millis(config.request_timeout_ms),
            max_response_bytes: config.max_response_bytes,
        }
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) policies: Arc<RwLock<PolicyCache>>,
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
            policies: Arc::new(RwLock::new(PolicyCache::default())),
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

fn env_u64(name: &str) -> Option<u64> {
    match env::var(name) {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(ms) => Some(ms),
            Err(err) => {
                warn!(%name, %value, error = %err, "invalid duration setting; ignoring environment value");
                None
            }
        },
        Err(_) => None,
    }
}

fn env_usize(name: &str) -> Option<usize> {
    match env::var(name) {
        Ok(value) => match value.trim().parse::<usize>() {
            Ok(parsed) => Some(parsed),
            Err(err) => {
                warn!(%name, %value, error = %err, "invalid numeric setting; ignoring environment value");
                None
            }
        },
        Err(_) => None,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "oneflowd=info".into()),
        )
        .init();

    let args = Args::parse();
    let config = DaemonConfig::load_default_or_explicit(args.config.as_deref())?;
    let config = DaemonConfig::resolve(&args, config)?;
    let settings = DaemonSettings::from_config(&config);
    control_request(&config.cp, &settings)?;
    let control_plane_url = config.cp;
    let listen_addr: SocketAddr = config
        .listen_addr
        .parse()
        .with_context(|| format!("invalid daemon listen_addr: {}", config.listen_addr))?;
    let cluster_name = env::var("CLUSTER_NAME").unwrap_or_else(|_| "cluster-ssh198".to_string());
    let hostname = env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string());

    let (telemetry_tx, telemetry_rx) = mpsc::channel(256);
    let state = AppState::new(settings, telemetry_tx)?;
    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    let bound_addr = listener.local_addr()?;

    info!(
        state = "starting",
        daemon_id = %state.settings.daemon_id,
        control_plane_url = %control_plane_url,
        listen_addr = %bound_addr,
        "starting oneflowd"
    );
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
        info!(state = "connecting", url = %control_plane_url, "connecting to control plane");
        let request = match control_request(&control_plane_url, &state.settings) {
            Ok(request) => request,
            Err(error) => {
                error!(%error, "invalid control connection credentials; control task stopped");
                return;
            }
        };
        match connect_async(request).await {
            Ok((mut socket, _)) => {
                info!(state = "connected", url = %control_plane_url, "connected to control plane");
                let registration = DaemonMessage::Registration(Registration {
                    daemon_id: state.settings.daemon_id.clone(),
                    cluster_name: cluster_name.clone(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    capabilities: vec![
                        "openai-compatible".to_string(),
                        "virtual-keys-v1".to_string(),
                    ],
                    hostname: hostname.clone(),
                });

                if send_daemon_message(&mut socket, &registration)
                    .await
                    .is_ok()
                {
                    info!(
                        state = "registered",
                        daemon_id = %state.settings.daemon_id,
                        "registered with control plane"
                    );
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
                                        if let Some(ack) = handle_control_message(&text, &state).await {
                                            if send_daemon_message(&mut socket, &ack).await.is_err() {
                                                break;
                                            }
                                        }
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
                } else {
                    warn!("failed to register with control plane");
                }
                warn!(
                    state = "reconnecting",
                    retry_in_secs = 5,
                    "control plane connection closed"
                );
            }
            Err(err) => {
                warn!(
                    state = "reconnecting",
                    url = %control_plane_url,
                    retry_in_secs = 5,
                    error = %err,
                    "control plane connection failed"
                )
            }
        }

        time::sleep(Duration::from_secs(5)).await;
    }
}

async fn handle_control_message(text: &str, state: &AppState) -> Option<DaemonMessage> {
    match serde_json::from_str::<ControlMessage>(text) {
        Ok(ControlMessage::VirtualKeyPolicySnapshot(snapshot)) => {
            let result =
                state
                    .policies
                    .write()
                    .await
                    .apply(snapshot, now_ts(), time::Instant::now());
            match result {
                Ok(ack) => {
                    info!(
                        revision = ack.revision,
                        valid_until = ack.valid_until,
                        "applied virtual-key policy"
                    );
                    return Some(DaemonMessage::VirtualKeyPolicyAck(ack));
                }
                Err(reason) => warn!(reason, "rejected virtual-key policy snapshot"),
            }
        }
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
    None
}

fn control_request(
    url: &str,
    settings: &DaemonSettings,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>> {
    let mut request = url.into_client_request()?;
    let token = &settings.control_token;
    let mut authorization: tokio_tungstenite::tungstenite::http::HeaderValue =
        format!("Bearer {token}")
            .parse()
            .context("ONEFLOW_DAEMON_TOKEN is not a valid HTTP header")?;
    authorization.set_sensitive(true);
    request.headers_mut().insert("authorization", authorization);
    request.headers_mut().insert(
        "x-oneflow-daemon-id",
        settings
            .daemon_id
            .parse()
            .context("daemon_id is not a valid HTTP header")?,
    );
    Ok(request)
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
    socket.send(Message::Text(json)).await.map_err(|err| {
        warn!(error = %err, "failed to send control channel message");
    })
}
