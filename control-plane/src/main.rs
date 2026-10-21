mod api;
mod control;
mod store;

use std::{
    net::SocketAddr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use axum::http::HeaderValue;
use protocol::ControlMessage;
use tokio::sync::broadcast;
use tracing::info;

use store::MemoryStore;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) store: Arc<MemoryStore>,
    pub(crate) broadcast: broadcast::Sender<ControlMessage>,
    pub(crate) started_at: i64,
}

pub(crate) fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
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
    let state = AppState {
        store: Arc::new(MemoryStore::new(store::default_config())),
        broadcast,
        started_at: now_ts(),
    };

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
