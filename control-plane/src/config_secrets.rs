//! Write-only handling of literal upstream credentials in model configuration.
//!
//! `parameters.api_key` must reach daemons, so the stored and broadcast
//! configuration keeps it. The management API never returns it: responses go
//! through [`redact`], and requests go through [`merge`] so an operator can
//! read a configuration, edit it, and send it back without re-entering keys.

use std::collections::HashMap;

use protocol::ModelConfigSet;

/// The parameter holding a literal upstream credential.
pub(crate) const API_KEY: &str = "api_key";

/// Strip every literal `api_key` and report whether one was stored.
pub(crate) fn redact(mut config: ModelConfigSet) -> ModelConfigSet {
    for model in &mut config.models {
        let stored = model
            .parameters
            .additional_properties
            .remove(API_KEY)
            .is_some_and(|key| !key.is_empty());
        model.api_key_configured = Some(stored);
    }
    config
}

/// Resolve an incoming configuration against the stored one.
///
/// Per model, matched by `model_name`:
/// - `api_key` omitted keeps the stored key, if any;
/// - `api_key` empty removes it;
/// - any other `api_key` replaces it.
///
/// `api_key_configured` is a response-only field and is cleared, so it is
/// never stored or broadcast to daemons.
pub(crate) fn merge(mut incoming: ModelConfigSet, current: &ModelConfigSet) -> ModelConfigSet {
    let stored: HashMap<&str, &String> = current
        .models
        .iter()
        .filter_map(|model| {
            model
                .parameters
                .get(API_KEY)
                .filter(|key| !key.is_empty())
                .map(|key| (model.model_name.as_str(), key))
        })
        .collect();
    for model in &mut incoming.models {
        model.api_key_configured = None;
        let parameters = &mut model.parameters.additional_properties;
        match parameters.get(API_KEY).map(String::as_str) {
            Some("") => {
                parameters.remove(API_KEY);
            }
            Some(_) => {}
            None => {
                if let Some(key) = stored.get(model.model_name.as_str()) {
                    parameters.insert(API_KEY.to_string(), (*key).clone());
                }
            }
        }
    }
    incoming
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{ModelConfig, ModelConfigParameters};

    fn model(name: &str, key: Option<&str>) -> ModelConfig {
        let mut parameters = ModelConfigParameters::new();
        parameters.insert("upstream_model", format!("{name}-upstream"));
        if let Some(key) = key {
            parameters.insert(API_KEY, key);
        }
        ModelConfig {
            model_name: name.to_string(),
            provider: "custom_openai".to_string(),
            api_base: "https://example.test/v1".to_string(),
            api_key_configured: None,
            parameters,
        }
    }

    fn set(models: Vec<ModelConfig>) -> ModelConfigSet {
        ModelConfigSet {
            version: "v".to_string(),
            models,
        }
    }

    fn key_of(config: &ModelConfigSet, name: &str) -> Option<String> {
        config
            .models
            .iter()
            .find(|model| model.model_name == name)
            .and_then(|model| model.parameters.get(API_KEY).cloned())
    }

    #[test]
    fn redact_removes_keys_and_reports_presence() {
        let redacted = redact(set(vec![
            model("with-key", Some("sk-provider-secret")),
            model("without-key", None),
            model("blank-key", Some("")),
        ]));
        assert!(!serde_json::to_string(&redacted)
            .unwrap()
            .contains("sk-provider-secret"));
        let flags: Vec<_> = redacted
            .models
            .iter()
            .map(|model| (model.parameters.get(API_KEY), model.api_key_configured))
            .collect();
        assert_eq!(
            flags,
            [(None, Some(true)), (None, Some(false)), (None, Some(false))]
        );
        assert_eq!(
            redacted.models[0].parameters.get("upstream_model").unwrap(),
            "with-key-upstream"
        );
    }

    #[test]
    fn merge_keeps_replaces_removes_and_does_not_leak_across_models() {
        let current = set(vec![
            model("kept", Some("old-kept")),
            model("replaced", Some("old-replaced")),
            model("removed", Some("old-removed")),
            model("dropped", Some("old-dropped")),
        ]);
        let mut incoming = redact(current.clone());
        incoming
            .models
            .retain(|model| model.model_name != "dropped");
        incoming.models[1]
            .parameters
            .insert(API_KEY, "new-replaced");
        incoming.models[2].parameters.insert(API_KEY, "");
        incoming.models.push(model("added", None));

        let merged = merge(incoming, &current);
        assert_eq!(key_of(&merged, "kept").as_deref(), Some("old-kept"));
        assert_eq!(key_of(&merged, "replaced").as_deref(), Some("new-replaced"));
        assert_eq!(key_of(&merged, "removed"), None);
        assert_eq!(key_of(&merged, "added"), None);
        assert!(merged.models.iter().all(|m| m.api_key_configured.is_none()));
        assert!(!serde_json::to_string(&merged)
            .unwrap()
            .contains("api_key_configured"));
    }

    #[test]
    fn renaming_a_model_does_not_carry_its_key() {
        let current = set(vec![model("old-name", Some("secret"))]);
        let mut incoming = redact(current.clone());
        incoming.models[0].model_name = "new-name".to_string();
        assert_eq!(key_of(&merge(incoming, &current), "new-name"), None);
    }
}
