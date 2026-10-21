use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

const CONFIG_PATH_ENV: &str = "LLMCP_CONFIG";
const DEFAULT_CONFIG_PATH: &str = "config.toml";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub control_plane: ControlPlaneConfig,
    pub web: WebConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneConfig {
    pub bind_addr: String,
    pub websocket_url: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    pub addr: String,
}

pub struct ConfigProvider;

impl ConfigProvider {
    pub fn load() -> Result<ServiceConfig> {
        let path = env::var_os(CONFIG_PATH_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH));
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("failed to read configuration file {}", path.display()))?;
        toml::from_str(&contents)
            .with_context(|| format!("failed to parse configuration file {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_service_addresses() {
        let config: ServiceConfig = toml::from_str(
            r#"
                [control_plane]
                bind_addr = "0.0.0.0:4000"
                websocket_url = "ws://127.0.0.1:4000/control"

                [web]
                addr = "127.0.0.1:5173"
            "#,
        )
        .unwrap();

        assert_eq!(config.control_plane.bind_addr, "0.0.0.0:4000");
        assert_eq!(
            config.control_plane.websocket_url,
            "ws://127.0.0.1:4000/control"
        );
        assert_eq!(config.web.addr, "127.0.0.1:5173");
    }

    #[test]
    fn rejects_unknown_fields() {
        let result = toml::from_str::<ServiceConfig>(
            r#"
                [control_plane]
                bind_addr = "0.0.0.0:4000"
                websocket_url = "ws://127.0.0.1:4000/control"
                typo = "not accepted"

                [web]
                addr = "127.0.0.1:5173"
            "#,
        );

        assert!(result.is_err());
    }
}
