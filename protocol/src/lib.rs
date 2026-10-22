//! Shared types for the control plane ⇄ daemon relationship.
//!
//! Two layers live here, and the distinction is deliberate:
//!
//! * [`generated`] — DTOs generated from `contract/openapi.yaml`, the source of
//!   truth for the HTTP API. Regenerate with
//!   `openapi-to-rust generate --config openapi-to-rust.toml`. Never hand-edit.
//! * The WebSocket control-channel messages below ([`ControlMessage`],
//!   [`DaemonMessage`], …). These are outside the HTTP contract (the contract
//!   documents the JSON API only), so they are hand-written. They reuse the
//!   generated [`ModelConfig`] rather than defining a second copy of it.

// `generated/` is emitted by `openapi-to-rust`; rustfmt must not rewrite it or
// the generator's `--check` mode would report the committed output as stale.
#[rustfmt::skip]
#[allow(clippy::double_must_use, reason = "Generated nullable-field builders repeat must_use on their return type")]
pub mod generated;

pub use generated::types::*;

use serde::{Deserialize, Serialize};

/// Hand-written conveniences over the generated `additionalProperties` newtype,
/// so call sites do not have to reach through `.additional_properties`.
impl ModelConfigParameters {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.additional_properties.insert(key.into(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&String> {
        self.additional_properties.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.additional_properties.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.additional_properties.is_empty()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum ControlMessage {
    VirtualKeyPolicySnapshot(VirtualKeyPolicySnapshot),
    ConfigUpdate {
        models: Vec<ModelConfig>,
        version: String,
    },
    Command {
        cmd: String,
        reason: String,
    },
}

impl From<ModelConfigSet> for ControlMessage {
    fn from(config: ModelConfigSet) -> Self {
        ControlMessage::ConfigUpdate {
            models: config.models,
            version: config.version,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum DaemonMessage {
    VirtualKeyPolicyAck(VirtualKeyPolicyAck),
    Registration(Registration),
    Heartbeat(Heartbeat),
    Telemetry(Telemetry),
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VirtualKeyPolicy {
    pub key_id: String,
    pub token_sha256: String,
    pub allowed_models: Vec<String>,
    pub expires_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VirtualKeyPolicySnapshot {
    pub protocol_version: u32,
    pub revision: i64,
    pub issued_at: i64,
    pub valid_until: i64,
    pub keys: Vec<VirtualKeyPolicy>,
}

impl std::fmt::Debug for VirtualKeyPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualKeyPolicy")
            .field("key_id", &self.key_id)
            .field("allowed_models", &self.allowed_models)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct VirtualKeyPolicyAck {
    pub protocol_version: u32,
    pub revision: i64,
    pub valid_until: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Registration {
    pub daemon_id: String,
    pub cluster_name: String,
    pub version: String,
    pub capabilities: Vec<String>,
    pub hostname: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Heartbeat {
    pub daemon_id: String,
    pub active_requests: u32,
    pub loaded_models: Vec<String>,
}

/// Per-request telemetry reported by a daemon after it calls an upstream
/// provider. The control plane stores these as [`UsageEvent`]s.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Telemetry {
    pub virtual_key_id: String,
    pub daemon_id: String,
    pub model_name: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub success: bool,
    pub request_id: String,
    pub latency_ms: i64,
    pub status_code: Option<u16>,
    pub streamed: bool,
    /// False when the upstream returned no `usage` object, so the token counts are
    /// unknown rather than genuinely zero.
    pub usage_reported: bool,
}

impl From<Telemetry> for UsageEvent {
    fn from(telemetry: Telemetry) -> Self {
        // Written as an exhaustive struct literal on purpose: a field added to
        // either side of the contract becomes a compile error here rather than
        // silently drifting.
        Self {
            virtual_key_id: telemetry.virtual_key_id,
            daemon_id: telemetry.daemon_id,
            model_name: telemetry.model_name,
            prompt_tokens: i64::from(telemetry.prompt_tokens),
            completion_tokens: i64::from(telemetry.completion_tokens),
            success: telemetry.success,
            request_id: telemetry.request_id,
            latency_ms: telemetry.latency_ms,
            status_code: telemetry.status_code.map(i64::from),
            streamed: telemetry.streamed,
            usage_reported: telemetry.usage_reported,
            // Receipt time is stamped by the control plane, which overwrites this.
            received_at: 0,
        }
    }
}

#[cfg(test)]
mod virtual_key_tests {
    use super::*;

    #[test]
    fn policy_wire_format_and_redacted_debug() {
        let message = ControlMessage::VirtualKeyPolicySnapshot(VirtualKeyPolicySnapshot {
            protocol_version: 1,
            revision: 2,
            issued_at: 1000,
            valid_until: 1300,
            keys: vec![VirtualKeyPolicy {
                key_id: "a".repeat(32),
                token_sha256: "b".repeat(64),
                allowed_models: vec!["alias".into()],
                expires_at: None,
            }],
        });
        let value = serde_json::to_value(&message).unwrap();
        assert_eq!(value["type"], "VirtualKeyPolicySnapshot");
        assert_eq!(value["protocol_version"], 1);
        assert!(value["keys"][0]["expires_at"].is_null());
        assert!(!format!("{message:?}").contains(&"b".repeat(64)));
        let parsed: ControlMessage = serde_json::from_value(value).unwrap();
        assert!(matches!(
            parsed,
            ControlMessage::VirtualKeyPolicySnapshot(_)
        ));
        let ack = DaemonMessage::VirtualKeyPolicyAck(VirtualKeyPolicyAck {
            protocol_version: 1,
            revision: 2,
            valid_until: 1300,
        });
        let value = serde_json::to_value(ack).unwrap();
        assert_eq!(value["type"], "VirtualKeyPolicyAck");
        assert_eq!(value["valid_until"], 1300);
    }

    #[test]
    fn telemetry_requires_virtual_key_attribution() {
        let mut value = serde_json::json!({
            "daemon_id": "daemon", "model_name": "alias", "prompt_tokens": 0,
            "completion_tokens": 0, "success": true, "request_id": "request",
            "latency_ms": 1, "status_code": 200, "streamed": false,
            "usage_reported": false
        });
        assert!(serde_json::from_value::<Telemetry>(value.clone()).is_err());
        value["virtual_key_id"] = serde_json::json!("a".repeat(32));
        let telemetry: Telemetry = serde_json::from_value(value).unwrap();
        let event = UsageEvent::from(telemetry);
        assert_eq!(event.virtual_key_id, "a".repeat(32));
    }
}
