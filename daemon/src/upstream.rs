//! Upstream request plumbing: usage accounting, timeouts, and the streaming
//! telemetry guard.

use std::time::Instant;

use axum::body::Bytes;
use futures_util::{stream, Stream, StreamExt};
use protocol::{ModelConfig, ModelConfigParameters, Telemetry};
use serde_json::{json, Value};

use crate::{ActiveRequestGuard, AppState};

/// Beyond this, an unterminated SSE line is not a usage frame; stop scanning
/// rather than growing memory without bound.
pub(crate) const MAX_SSE_SCAN_BUFFER: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Usage {
    pub(crate) prompt_tokens: u32,
    pub(crate) completion_tokens: u32,
}

impl Usage {
    /// Extracts `usage` from an OpenAI-shaped payload.
    ///
    /// Returns `None` when the field is absent or explicitly null — streaming
    /// providers send `"usage": null` on every chunk except the last one, and a
    /// provider that omits usage entirely must be distinguishable from one that
    /// reported zeros.
    pub(crate) fn from_json(value: &Value) -> Option<Self> {
        let usage = value.get("usage")?;
        if usage.is_null() {
            return None;
        }
        let prompt = usage.get("prompt_tokens")?.as_u64()?;
        let completion = usage.get("completion_tokens")?.as_u64()?;
        Some(Self {
            prompt_tokens: u32::try_from(prompt).unwrap_or(u32::MAX),
            completion_tokens: u32::try_from(completion).unwrap_or(u32::MAX),
        })
    }

    pub(crate) fn from_body(body: &[u8]) -> Option<Self> {
        let value: Value = serde_json::from_slice(body).ok()?;
        Self::from_json(&value)
    }
}

/// Accumulates SSE `data:` payloads looking for the trailing usage chunk. Frames
/// may be split across arbitrary chunk boundaries, so bytes are buffered until a
/// newline completes a line.
#[derive(Default)]
pub(crate) struct UsageScanner {
    buffer: Vec<u8>,
    usage: Option<Usage>,
    disabled: bool,
}

impl UsageScanner {
    pub(crate) fn scan(&mut self, chunk: &[u8]) {
        if self.disabled {
            return;
        }
        self.buffer.extend_from_slice(chunk);

        while let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=position).collect();
            let line = line.strip_suffix(b"\n").unwrap_or(line.as_slice());
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            self.scan_line(line);
        }

        if self.buffer.len() > MAX_SSE_SCAN_BUFFER {
            self.disabled = true;
            self.buffer.clear();
        }
    }

    fn scan_line(&mut self, line: &[u8]) {
        let Some(data) = line.strip_prefix(b"data:") else {
            return;
        };
        let data = trim_ascii(data);
        if data.is_empty() || data == b"[DONE]" {
            return;
        }
        let Ok(value) = serde_json::from_slice::<Value>(data) else {
            return;
        };
        if let Some(usage) = Usage::from_json(&value) {
            self.usage = Some(usage);
        }
    }

    pub(crate) fn usage(&self) -> Option<Usage> {
        self.usage
    }
}

fn trim_ascii(data: &[u8]) -> &[u8] {
    let mut data = data;
    while let [first, rest @ ..] = data {
        if first.is_ascii_whitespace() {
            data = rest;
        } else {
            break;
        }
    }
    data
}

/// Everything needed to describe one upstream call to the control plane.
pub(crate) struct RequestContext {
    pub(crate) virtual_key_id: String,
    pub(crate) daemon_id: String,
    pub(crate) model_name: String,
    pub(crate) request_id: String,
    pub(crate) streamed: bool,
    pub(crate) started: Instant,
    pub(crate) status_code: Option<u16>,
}

impl RequestContext {
    pub(crate) fn telemetry(&self, usage: Option<Usage>, success: bool) -> Telemetry {
        Telemetry {
            virtual_key_id: self.virtual_key_id.clone(),
            daemon_id: self.daemon_id.clone(),
            model_name: self.model_name.clone(),
            prompt_tokens: usage.map_or(0, |usage| usage.prompt_tokens),
            completion_tokens: usage.map_or(0, |usage| usage.completion_tokens),
            success,
            request_id: self.request_id.clone(),
            latency_ms: i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX),
            status_code: self.status_code,
            streamed: self.streamed,
            usage_reported: usage.is_some(),
        }
    }
}

/// Reports telemetry exactly once per request, whether the stream completes,
/// fails, or is dropped because the client disconnected mid-body.
pub(crate) struct TelemetryGuard {
    state: AppState,
    context: Option<RequestContext>,
    scanner: UsageScanner,
    /// Held so a streaming request keeps counting as in-flight until the body ends.
    _active: ActiveRequestGuard,
}

impl TelemetryGuard {
    pub(crate) fn new(
        state: AppState,
        context: RequestContext,
        active: ActiveRequestGuard,
    ) -> Self {
        Self {
            state,
            context: Some(context),
            scanner: UsageScanner::default(),
            _active: active,
        }
    }

    pub(crate) fn scan(&mut self, chunk: &[u8]) {
        self.scanner.scan(chunk);
    }

    pub(crate) fn finish(&mut self, success: bool) {
        if let Some(context) = self.context.take() {
            let telemetry = context.telemetry(self.scanner.usage(), success);
            self.state.report(telemetry);
        }
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        // Reached when the client goes away before the stream ends. The upstream
        // call did not complete, so it is recorded as a failure carrying whatever
        // usage had been observed.
        self.finish(false);
    }
}

/// Wraps the upstream byte stream so chunks reach the client untouched while a
/// telemetry guard watches them.
pub(crate) fn streaming_body(
    guard: TelemetryGuard,
    inner: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
    stream::unfold(
        (Box::pin(inner), guard),
        |(mut inner, mut guard)| async move {
            match inner.next().await {
                Some(Ok(chunk)) => {
                    guard.scan(&chunk);
                    Some((Ok(chunk), (inner, guard)))
                }
                Some(Err(err)) => {
                    guard.finish(false);
                    Some((Err(std::io::Error::other(err)), (inner, guard)))
                }
                None => {
                    guard.finish(true);
                    None
                }
            }
        },
    )
}

pub(crate) fn chat_completions_endpoint(api_base: &str) -> String {
    let base = api_base.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else {
        format!("{base}/chat/completions")
    }
}

pub(crate) fn apply_model_parameters(payload: &mut Value, parameters: &ModelConfigParameters) {
    let Some(object) = payload.as_object_mut() else {
        return;
    };

    if let Some(upstream_model) = parameters.get("upstream_model") {
        object.insert("model".to_string(), Value::String(upstream_model.clone()));
    }

    for (name, value) in parameters.iter() {
        if matches!(name.as_str(), "api_key" | "api_key_env" | "upstream_model") {
            continue;
        }
        object.entry(name.clone()).or_insert_with(|| {
            serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.clone()))
        });
    }
}

/// Providers only emit a final `usage` chunk when asked. Enabled for OpenAI-shaped
/// providers, opt out per model with `include_usage = "false"`.
pub(crate) fn enable_stream_usage(payload: &mut Value, model: &ModelConfig) {
    if model.provider != "custom_openai" {
        return;
    }
    if model.parameters.get("include_usage").map(String::as_str) == Some("false") {
        return;
    }

    let Some(object) = payload.as_object_mut() else {
        return;
    };
    let options = object
        .entry("stream_options".to_string())
        .or_insert_with(|| json!({}));
    if !options.is_object() {
        *options = json!({});
    }
    if let Some(options) = options.as_object_mut() {
        options.insert("include_usage".to_string(), Value::Bool(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_chat_completions_endpoint() {
        assert_eq!(
            chat_completions_endpoint("https://provider.example/v1/"),
            "https://provider.example/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_endpoint("https://provider.example/v1/chat/completions"),
            "https://provider.example/v1/chat/completions"
        );
    }

    #[test]
    fn applies_defaults_without_overwriting_request_values() {
        let mut payload = json!({"model": "alias", "temperature": 0.2});
        let mut parameters = ModelConfigParameters::new();
        parameters.insert("upstream_model", "real-model");
        parameters.insert("temperature", "0.8");
        parameters.insert("max_tokens", "128");
        parameters.insert("api_key", "secret");

        apply_model_parameters(&mut payload, &parameters);

        assert_eq!(payload["model"], "real-model");
        assert_eq!(payload["temperature"], 0.2);
        assert_eq!(payload["max_tokens"], 128);
        assert!(payload.get("api_key").is_none());
    }

    #[test]
    fn null_usage_is_not_treated_as_zero() {
        assert_eq!(Usage::from_json(&json!({"usage": null})), None);
        assert_eq!(Usage::from_json(&json!({"id": "x"})), None);
        assert_eq!(
            Usage::from_json(&json!({"usage": {"prompt_tokens": 0, "completion_tokens": 0}})),
            Some(Usage::default())
        );
    }

    #[test]
    fn usage_scanner_handles_frames_split_across_chunks() {
        let mut scanner = UsageScanner::default();

        scanner.scan(b"data: {\"choices\":[{\"delta\":{}}]}\n\ndata: {\"us");
        assert_eq!(scanner.usage(), None, "incomplete frame must not be parsed");

        scanner.scan(b"age\":{\"prompt_tokens\":3,\"completion_tokens\":4}}\n\n");
        assert_eq!(
            scanner.usage(),
            Some(Usage {
                prompt_tokens: 3,
                completion_tokens: 4
            })
        );
    }

    #[test]
    fn usage_scanner_ignores_non_data_lines_and_done_marker() {
        let mut scanner = UsageScanner::default();
        scanner.scan(b": keep-alive\n\nevent: ping\n\ndata: [DONE]\n\n");
        assert_eq!(scanner.usage(), None);
    }

    #[test]
    fn stream_usage_injection_respects_provider_and_opt_out() {
        let mut model = ModelConfig {
            model_name: "m".into(),
            provider: "custom_openai".into(),
            api_base: "https://example.test/v1".into(),
            parameters: ModelConfigParameters::new(),
        };

        let mut payload = json!({"stream": true});
        enable_stream_usage(&mut payload, &model);
        assert_eq!(payload["stream_options"]["include_usage"], true);

        model.parameters.insert("include_usage", "false");
        let mut payload = json!({"stream": true});
        enable_stream_usage(&mut payload, &model);
        assert!(payload.get("stream_options").is_none());

        model.provider = "other".into();
        model.parameters = ModelConfigParameters::new();
        let mut payload = json!({"stream": true});
        enable_stream_usage(&mut payload, &model);
        assert!(payload.get("stream_options").is_none());
    }
}
