//! In-memory state for the control plane.
//!
//! Every read and write goes through [`MemoryStore`] so that handlers never touch
//! the underlying collections directly. ADR 0001 replaces this with PostgreSQL;
//! keeping the method set narrow and async is what makes that swap local.

use std::collections::{BTreeMap, VecDeque};

use protocol::{
    Daemon, DaemonMessage, ModelConfig, ModelConfigParameters, ModelConfigSet, UsageEvent,
};
use tokio::sync::RwLock;
use tracing::info;

/// Number of usage events retained for `GET /v1/usage/events`.
pub const USAGE_HISTORY_LIMIT: usize = 200;

pub fn default_config() -> ModelConfigSet {
    ModelConfigSet {
        version: "poc-v1".to_string(),
        models: vec![
            ModelConfig {
                model_name: "deepseek-v4-pro".to_string(),
                provider: "custom_openai".to_string(),
                api_base: "https://api.deepseek.com/v1".to_string(),
                parameters: ModelConfigParameters::new(),
            },
            ModelConfig {
                model_name: "deepseek-v4-flash".to_string(),
                provider: "custom_openai".to_string(),
                api_base: "https://api.deepseek.com/v1".to_string(),
                parameters: ModelConfigParameters::new(),
            },
        ],
    }
}

/// Filters for [`MemoryStore::usage_events`].
#[derive(Debug, Default)]
pub struct UsageQuery {
    pub limit: usize,
    /// Return only events older than this sequence number (exclusive).
    pub before: Option<u64>,
    pub daemon_id: Option<String>,
    pub model_name: Option<String>,
}

#[derive(Debug)]
pub struct UsagePage {
    pub items: Vec<UsageEvent>,
    /// Sequence number to pass as `before` for the next (older) page.
    pub next_before: Option<u64>,
}

struct Inner {
    config: ModelConfigSet,
    /// Keyed by `connection_id`; a `BTreeMap` keeps list output stable.
    daemons: BTreeMap<String, Daemon>,
    /// `(sequence, event)`, oldest at the front.
    usage: VecDeque<(u64, UsageEvent)>,
    next_sequence: u64,
}

pub struct MemoryStore {
    inner: RwLock<Inner>,
}

impl MemoryStore {
    pub fn new(config: ModelConfigSet) -> Self {
        Self {
            inner: RwLock::new(Inner {
                config,
                daemons: BTreeMap::new(),
                usage: VecDeque::new(),
                next_sequence: 1,
            }),
        }
    }

    pub async fn config(&self) -> ModelConfigSet {
        self.inner.read().await.config.clone()
    }

    pub async fn set_config(&self, config: ModelConfigSet) {
        self.inner.write().await.config = config;
    }

    /// Returns `(config_version, connected_daemons)` from one consistent read.
    pub async fn summary(&self) -> (String, usize) {
        let inner = self.inner.read().await;
        (inner.config.version.clone(), inner.daemons.len())
    }

    pub async fn daemon_connected(&self, connection_id: &str, now: i64) {
        self.inner
            .write()
            .await
            .daemons
            .insert(connection_id.to_string(), new_daemon(connection_id, now));
    }

    pub async fn daemon_disconnected(&self, connection_id: &str) {
        self.inner.write().await.daemons.remove(connection_id);
    }

    pub async fn daemons(&self) -> Vec<Daemon> {
        self.inner.read().await.daemons.values().cloned().collect()
    }

    pub async fn daemon(&self, connection_id: &str) -> Option<Daemon> {
        self.inner.read().await.daemons.get(connection_id).cloned()
    }

    /// Applies one message received on a daemon's control channel. Messages for
    /// a connection that is no longer registered are ignored.
    pub async fn apply_daemon_message(
        &self,
        connection_id: &str,
        message: DaemonMessage,
        now: i64,
    ) {
        let mut inner = self.inner.write().await;
        let inner = &mut *inner;

        let Some(daemon) = inner.daemons.get_mut(connection_id) else {
            return;
        };
        daemon.last_seen = now;

        match message {
            DaemonMessage::VirtualKeyPolicyAck(ack) => {
                if ack.protocol_version == 1 {
                    daemon.auth_policy_revision = Some(ack.revision);
                    daemon.auth_policy_valid_until = Some(ack.valid_until);
                }
            }
            DaemonMessage::Registration(reg) => {
                info!(
                    daemon_id = %reg.daemon_id,
                    cluster = %reg.cluster_name,
                    "Daemon registered"
                );
                daemon.daemon_id = Some(reg.daemon_id);
                daemon.cluster_name = Some(reg.cluster_name);
                daemon.version = Some(reg.version);
                daemon.hostname = Some(reg.hostname);
                daemon.capabilities = reg.capabilities;
            }
            DaemonMessage::Heartbeat(heartbeat) => {
                daemon.active_requests = i64::from(heartbeat.active_requests);
                daemon.loaded_models = heartbeat.loaded_models;
            }
            DaemonMessage::Telemetry(telemetry) => {
                daemon.total_requests += 1;
                daemon.total_prompt_tokens += i64::from(telemetry.prompt_tokens);
                daemon.total_completion_tokens += i64::from(telemetry.completion_tokens);
                if !telemetry.success {
                    daemon.failed_requests += 1;
                }

                let mut event = UsageEvent::from(telemetry);
                event.received_at = now;
                let sequence = inner.next_sequence;
                inner.next_sequence += 1;
                inner.usage.push_back((sequence, event));
                while inner.usage.len() > USAGE_HISTORY_LIMIT {
                    inner.usage.pop_front();
                }
            }
        }
    }

    /// Newest-first page of usage events matching `query`.
    pub async fn usage_events(&self, query: &UsageQuery) -> UsagePage {
        let inner = self.inner.read().await;
        let mut matching = inner
            .usage
            .iter()
            .rev()
            .filter(|(sequence, _)| query.before.is_none_or(|before| *sequence < before))
            .filter(|(_, event)| {
                query
                    .daemon_id
                    .as_deref()
                    .is_none_or(|id| event.daemon_id == id)
            })
            .filter(|(_, event)| {
                query
                    .model_name
                    .as_deref()
                    .is_none_or(|name| event.model_name == name)
            });

        let mut items = Vec::with_capacity(query.limit);
        let mut last_sequence = None;
        for (sequence, event) in matching.by_ref().take(query.limit) {
            items.push(event.clone());
            last_sequence = Some(*sequence);
        }
        let has_more = matching.next().is_some();

        UsagePage {
            items,
            next_before: if has_more { last_sequence } else { None },
        }
    }
}

fn new_daemon(connection_id: &str, now: i64) -> Daemon {
    Daemon {
        connection_id: connection_id.to_string(),
        daemon_id: None,
        cluster_name: None,
        version: None,
        hostname: None,
        capabilities: Vec::new(),
        connected_at: now,
        last_seen: now,
        active_requests: 0,
        loaded_models: Vec::new(),
        total_requests: 0,
        failed_requests: 0,
        total_prompt_tokens: 0,
        total_completion_tokens: 0,
        auth_policy_revision: None,
        auth_policy_valid_until: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{Telemetry, VirtualKeyPolicyAck};

    fn telemetry(daemon_id: &str, model_name: &str, request_id: &str) -> DaemonMessage {
        DaemonMessage::Telemetry(Telemetry {
            virtual_key_id: None,
            daemon_id: daemon_id.to_string(),
            model_name: model_name.to_string(),
            prompt_tokens: 1,
            completion_tokens: 2,
            success: true,
            request_id: request_id.to_string(),
            latency_ms: 10,
            status_code: Some(200),
            streamed: false,
            usage_reported: true,
        })
    }

    /// The contract declares `daemon_id` and friends as `type: [string, "null"]`,
    /// so an unregistered daemon must serialize explicit nulls rather than
    /// omitting the keys.
    #[test]
    fn nullable_daemon_fields_serialize_as_present_nulls() {
        let value = serde_json::to_value(new_daemon("conn-1", 0)).unwrap();

        for field in ["daemon_id", "cluster_name", "version", "hostname"] {
            assert!(value.get(field).is_some(), "`{field}` must be present");
            assert!(value[field].is_null(), "`{field}` should serialize as null");
        }
    }

    #[tokio::test]
    async fn usage_history_is_bounded_and_paged_newest_first() {
        let store = MemoryStore::new(default_config());
        store.daemon_connected("conn", 0).await;
        for index in 0..(USAGE_HISTORY_LIMIT + 5) {
            store
                .apply_daemon_message("conn", telemetry("d", "m", &format!("req-{index}")), 0)
                .await;
        }

        let mut seen = Vec::new();
        let mut before = None;
        loop {
            let page = store
                .usage_events(&UsageQuery {
                    limit: 64,
                    before,
                    ..UsageQuery::default()
                })
                .await;
            seen.extend(page.items.into_iter().map(|event| event.request_id));
            match page.next_before {
                Some(next) => before = Some(next),
                None => break,
            }
        }

        assert_eq!(seen.len(), USAGE_HISTORY_LIMIT);
        assert_eq!(
            seen.first().unwrap(),
            &format!("req-{}", USAGE_HISTORY_LIMIT + 4)
        );
        assert_eq!(seen.last().unwrap(), "req-5");
    }

    #[tokio::test]
    async fn usage_filters_apply_before_the_limit() {
        let store = MemoryStore::new(default_config());
        store.daemon_connected("conn", 0).await;
        store
            .apply_daemon_message("conn", telemetry("a", "m1", "1"), 0)
            .await;
        store
            .apply_daemon_message("conn", telemetry("b", "m1", "2"), 0)
            .await;
        store
            .apply_daemon_message("conn", telemetry("a", "m2", "3"), 0)
            .await;

        let page = store
            .usage_events(&UsageQuery {
                limit: 1,
                daemon_id: Some("a".to_string()),
                ..UsageQuery::default()
            })
            .await;
        assert_eq!(page.items[0].request_id, "3");
        let next = store
            .usage_events(&UsageQuery {
                limit: 1,
                before: page.next_before,
                daemon_id: Some("a".to_string()),
                ..UsageQuery::default()
            })
            .await;
        assert_eq!(next.items[0].request_id, "1");
        assert!(next.next_before.is_none());

        let by_model = store
            .usage_events(&UsageQuery {
                limit: 10,
                model_name: Some("m1".to_string()),
                ..UsageQuery::default()
            })
            .await;
        assert_eq!(by_model.items.len(), 2);
    }

    #[tokio::test]
    async fn messages_for_unknown_connections_are_ignored() {
        let store = MemoryStore::new(default_config());
        store
            .apply_daemon_message("ghost", telemetry("d", "m", "1"), 0)
            .await;

        assert!(store
            .usage_events(&UsageQuery {
                limit: 10,
                ..UsageQuery::default()
            })
            .await
            .items
            .is_empty());
    }

    #[tokio::test]
    async fn virtual_key_policy_ack_is_exposed_on_daemon_status() {
        let store = MemoryStore::new(default_config());
        store.daemon_connected("conn", 1).await;
        store
            .apply_daemon_message(
                "conn",
                DaemonMessage::VirtualKeyPolicyAck(VirtualKeyPolicyAck {
                    protocol_version: 1,
                    revision: 42,
                    valid_until: 345,
                }),
                2,
            )
            .await;
        let daemon = store.daemon("conn").await.unwrap();
        assert_eq!(daemon.auth_policy_revision, Some(42));
        assert_eq!(daemon.auth_policy_valid_until, Some(345));
    }
}
