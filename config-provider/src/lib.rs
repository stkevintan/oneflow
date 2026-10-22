use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

const CONFIG_PATH_ENV: &str = "LLMCP_CONFIG";
const DEFAULT_CONFIG_PATH: &str = "config.toml";

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub control_plane: ControlPlaneConfig,
    pub web: WebConfig,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneConfig {
    pub bind_addr: String,
    pub websocket_url: String,
    /// Browser origins allowed to call the `/v1` API cross-origin. Empty means
    /// same-origin only (for example through the web dev/preview proxy).
    #[serde(default)]
    pub cors_allowed_origins: Vec<String>,
    pub database_url: Option<String>,
    pub admin_token: Option<String>,
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
        let contents = fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to read configuration file {}; copy config.example.toml to config.toml or set LLMCP_CONFIG",
                path.display()
            )
        })?;
        toml::from_str(&contents).map_err(|err: toml::de::Error| {
            anyhow::anyhow!(
                "failed to parse configuration file {}: {}",
                path.display(),
                err.message()
            )
        })
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
        assert!(config.control_plane.cors_allowed_origins.is_empty());
        assert!(config.control_plane.database_url.is_none());
        assert!(config.control_plane.admin_token.is_none());
    }

    #[test]
    fn parses_virtual_key_settings() {
        let config: ServiceConfig = toml::from_str(
            r#"
                [control_plane]
                bind_addr = "127.0.0.1:4000"
                websocket_url = "ws://127.0.0.1:4000/control"
                database_url = "postgres://app:placeholder@localhost/app"
                admin_token = "test-admin"
                [web]
                addr = "127.0.0.1:5173"
            "#,
        )
        .unwrap();
        assert_eq!(
            config.control_plane.database_url.as_deref(),
            Some("postgres://app:placeholder@localhost/app")
        );
        assert_eq!(
            config.control_plane.admin_token.as_deref(),
            Some("test-admin")
        );
    }

    #[test]
    fn example_config_is_valid_legacy_mode() {
        let config: ServiceConfig =
            toml::from_str(include_str!("../../config.example.toml")).unwrap();
        assert!(config.control_plane.database_url.is_none());
        assert!(config.control_plane.admin_token.is_none());
    }

    #[test]
    fn parses_cors_allowed_origins() {
        let config: ServiceConfig = toml::from_str(
            r#"
                [control_plane]
                bind_addr = "0.0.0.0:4000"
                websocket_url = "ws://127.0.0.1:4000/control"
                cors_allowed_origins = ["http://127.0.0.1:5173", "tauri://localhost"]

                [web]
                addr = "127.0.0.1:5173"
            "#,
        )
        .unwrap();

        assert_eq!(
            config.control_plane.cors_allowed_origins,
            ["http://127.0.0.1:5173", "tauri://localhost"]
        );
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

    #[test]
    fn rejects_removed_static_daemon_tokens() {
        let result = toml::from_str::<ServiceConfig>(
            r#"
                [control_plane]
                bind_addr = "127.0.0.1:4000"
                websocket_url = "ws://127.0.0.1:4000/control"
                daemon_tokens = { daemon-1 = "old-token" }
                [web]
                addr = "127.0.0.1:5173"
            "#,
        );
        assert!(result.is_err());
    }
}
