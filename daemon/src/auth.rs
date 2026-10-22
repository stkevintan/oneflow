use std::{collections::HashSet, time::Duration};

use protocol::{VirtualKeyPolicy, VirtualKeyPolicyAck, VirtualKeyPolicySnapshot};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::time::Instant;

#[derive(Default)]
pub(crate) struct PolicyCache {
    snapshot: Option<VirtualKeyPolicySnapshot>,
    deadline: Option<Instant>,
}

#[derive(Debug)]
pub(crate) enum AuthFailure {
    InvalidKey,
    Unavailable,
}

#[derive(Clone)]
pub(crate) struct Principal {
    pub key_id: String,
    pub allowed_models: Vec<String>,
}

impl Principal {
    pub fn allows(&self, model: &str) -> bool {
        self.allowed_models.iter().any(|allowed| allowed == model)
    }
}

pub(crate) fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn token_id(token: &str) -> Option<&str> {
    let rest = token
        .strip_prefix("sk-")
        .or_else(|| token.strip_prefix("ofk_"))?;
    let (id, secret) = rest.split_once('_')?;
    (is_hex(id, 32) && is_hex(secret, 64)).then_some(id)
}

impl PolicyCache {
    pub fn apply(
        &mut self,
        mut incoming: VirtualKeyPolicySnapshot,
        now: i64,
        monotonic_now: Instant,
    ) -> Result<VirtualKeyPolicyAck, &'static str> {
        if incoming.protocol_version != 1
            || incoming.revision < 0
            || incoming.issued_at < 0
            || incoming.issued_at >= incoming.valid_until
            || incoming.valid_until.saturating_sub(incoming.issued_at) > 300
            || incoming.valid_until <= now
            || incoming.issued_at > now.saturating_add(30)
        {
            return Err("invalid policy version, revision, or lease");
        }
        let mut ids = HashSet::new();
        for policy in &mut incoming.keys {
            if !is_hex(&policy.key_id, 32)
                || !is_hex(&policy.token_sha256, 64)
                || !ids.insert(policy.key_id.clone())
                || policy.allowed_models.is_empty()
                || policy.expires_at.is_some_and(|expiry| expiry < 0)
            {
                return Err("invalid or duplicate policy key");
            }
            let mut models = HashSet::new();
            for model in &policy.allowed_models {
                if model.trim().is_empty() || model.contains('*') || !models.insert(model.clone()) {
                    return Err("invalid model allowlist");
                }
            }
            policy.allowed_models.sort();
        }
        incoming.keys.sort_by(|a, b| a.key_id.cmp(&b.key_id));
        if let Some(previous) = &self.snapshot {
            if incoming.revision < previous.revision {
                return Err("policy revision moved backwards");
            }
            if incoming.revision == previous.revision
                && (incoming.keys != previous.keys
                    || incoming.issued_at <= previous.issued_at
                    || incoming.valid_until <= previous.valid_until)
            {
                return Err("policy replay or conflicting equal revision");
            }
            if incoming.issued_at < previous.issued_at {
                return Err("policy issuance moved backwards");
            }
        }
        let ack = VirtualKeyPolicyAck {
            protocol_version: 1,
            revision: incoming.revision,
            valid_until: incoming.valid_until,
        };
        self.deadline =
            Some(monotonic_now + Duration::from_secs((incoming.valid_until - now) as u64));
        self.snapshot = Some(incoming);
        Ok(ack)
    }

    pub fn authenticate(
        &self,
        token: &str,
        now: i64,
        monotonic_now: Instant,
    ) -> Result<Principal, AuthFailure> {
        let id = token_id(token).ok_or(AuthFailure::InvalidKey)?;
        let snapshot = self
            .snapshot
            .as_ref()
            .filter(|snapshot| snapshot.valid_until > now)
            .filter(|_| {
                self.deadline
                    .is_some_and(|deadline| monotonic_now < deadline)
            })
            .ok_or(AuthFailure::Unavailable)?;
        let policy: &VirtualKeyPolicy = snapshot
            .keys
            .iter()
            .find(|policy| policy.key_id == id)
            .ok_or(AuthFailure::InvalidKey)?;
        let digest = format!("{:x}", Sha256::digest(token.as_bytes()));
        if !bool::from(digest.as_bytes().ct_eq(policy.token_sha256.as_bytes()))
            || policy.expires_at.is_some_and(|expiry| now >= expiry)
        {
            return Err(AuthFailure::InvalidKey);
        }
        Ok(Principal {
            key_id: id.to_string(),
            allowed_models: policy.allowed_models.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> String {
        format!("ofk_{}_{}", "a".repeat(32), "b".repeat(64))
    }

    fn snapshot() -> VirtualKeyPolicySnapshot {
        VirtualKeyPolicySnapshot {
            protocol_version: 1,
            revision: 1,
            issued_at: 1000,
            valid_until: 1300,
            keys: vec![VirtualKeyPolicy {
                key_id: "a".repeat(32),
                token_sha256: format!("{:x}", Sha256::digest(token().as_bytes())),
                allowed_models: vec!["alias".into()],
                expires_at: None,
            }],
        }
    }

    #[test]
    fn expiry_and_monotonic_clock_fence_access() {
        let instant = Instant::now();
        let mut cache = PolicyCache::default();
        assert!(matches!(
            cache.authenticate(&token(), 1000, instant),
            Err(AuthFailure::Unavailable)
        ));
        cache.apply(snapshot(), 1000, instant).unwrap();
        assert!(cache.authenticate(&token(), 1299, instant).is_ok());
        assert!(matches!(
            cache.authenticate(&token(), 1300, instant),
            Err(AuthFailure::Unavailable)
        ));
        assert!(matches!(
            cache.authenticate(&token(), 900, instant + Duration::from_secs(300)),
            Err(AuthFailure::Unavailable)
        ));
        let mut expired = snapshot();
        expired.keys[0].expires_at = Some(1001);
        let mut cache = PolicyCache::default();
        cache.apply(expired, 1000, instant).unwrap();
        assert!(cache.authenticate(&token(), 1000, instant).is_ok());
        assert!(matches!(
            cache.authenticate(&token(), 1001, instant),
            Err(AuthFailure::InvalidKey)
        ));
    }

    #[test]
    fn snapshot_validation_replay_renewal_and_revocation() {
        let instant = Instant::now();
        let mut cache = PolicyCache::default();
        cache.apply(snapshot(), 1000, instant).unwrap();
        assert!(cache.apply(snapshot(), 1000, instant).is_err());
        let mut bad = snapshot();
        bad.valid_until = 1301;
        assert!(cache.apply(bad, 1000, instant).is_err());
        let mut bad = snapshot();
        bad.keys.push(bad.keys[0].clone());
        bad.revision = 2;
        assert!(cache.apply(bad, 1000, instant).is_err());
        let mut bad = snapshot();
        bad.revision = 0;
        assert!(cache.apply(bad, 1000, instant).is_err());
        let mut renewal = snapshot();
        renewal.issued_at += 60;
        renewal.valid_until += 60;
        cache.apply(renewal.clone(), 1060, instant).unwrap();
        renewal.revision += 1;
        renewal.keys.clear();
        cache.apply(renewal, 1060, instant).unwrap();
        assert!(matches!(
            cache.authenticate(&token(), 1060, instant),
            Err(AuthFailure::InvalidKey)
        ));
    }

    #[test]
    fn skew_boundary_and_conflicting_equal_revision() {
        let instant = Instant::now();
        let mut cache = PolicyCache::default();
        assert!(cache.apply(snapshot(), 969, instant).is_err());
        cache.apply(snapshot(), 970, instant).unwrap();
        let mut conflict = snapshot();
        conflict.issued_at += 60;
        conflict.valid_until += 60;
        conflict.keys.clear();
        assert!(cache.apply(conflict, 1060, instant).is_err());
        assert!(cache
            .authenticate(&token(), 1000, instant)
            .unwrap()
            .allows("alias"));
        assert!(token_id("invalid").is_none());
        assert_eq!(token_id(&token()), Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        let new_token = token().replacen("ofk_", "sk-", 1);
        assert_eq!(
            token_id(&new_token),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
    }
}
