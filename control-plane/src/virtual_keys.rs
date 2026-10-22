use std::{collections::HashSet, str::FromStr};

use anyhow::{Context, Result};
use protocol::{ModelConfigSet, VirtualKeyPolicy, VirtualKeyPolicySnapshot};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use sqlx::{
    postgres::{PgConnectOptions, PgListener, PgPoolOptions},
    PgPool, Row,
};

const POLICY_LEASE_SECONDS: i64 = 300;
const ADVISORY_LOCK_ID: i64 = 0x4f4e_4546_4c4f_5701;

pub(crate) fn identifier_hash(identifier: &str) -> String {
    if identifier.starts_with("sk-") || identifier.starts_with("ofk_") {
        hex::encode(Sha256::digest(identifier.as_bytes()))
    } else {
        identifier.to_string()
    }
}

pub(crate) struct CreateVirtualKey {
    pub name: String,
    pub allowed_models: Vec<String>,
    pub expires_at: Option<i64>,
}

pub(crate) struct VirtualKey {
    pub id: String,
    pub name: String,
    pub allowed_models: Vec<String>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

pub(crate) struct CreatedVirtualKey {
    pub key: VirtualKey,
    pub secret: String,
}

pub struct VirtualKeyStore {
    pool: PgPool,
}

impl VirtualKeyStore {
    pub async fn connect(database_url: &str, initial_config: &ModelConfigSet) -> Result<Self> {
        let options = PgConnectOptions::from_str(database_url)
            .context("ONEFLOW_DATABASE_URL is not a valid PostgreSQL URL")?;
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect_with(options)
            .await
            .context("failed to connect to the virtual-key PostgreSQL database")?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("failed to apply virtual-key database migrations")?;

        sqlx::query(
            "INSERT INTO virtual_model_config (singleton, config) VALUES (TRUE, $1) \
             ON CONFLICT (singleton) DO NOTHING",
        )
        .bind(serde_json::to_value(initial_config)?)
        .execute(&pool)
        .await
        .context("failed to initialize durable model configuration")?;

        Ok(Self { pool })
    }

    pub async fn config(&self) -> Result<ModelConfigSet> {
        let value: serde_json::Value =
            sqlx::query_scalar("SELECT config FROM virtual_model_config WHERE singleton = TRUE")
                .fetch_one(&self.pool)
                .await
                .context("failed to read durable model configuration")?;
        serde_json::from_value(value).context("durable model configuration is invalid")
    }

    /// Serialize model replacement with key creation so allowlist validation
    /// cannot race a configuration change.
    pub async fn set_config(&self, config: &ModelConfigSet) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        lock_mutations(&mut tx).await?;
        sqlx::query("UPDATE virtual_model_config SET config = $1 WHERE singleton = TRUE")
            .bind(serde_json::to_value(config)?)
            .execute(&mut *tx)
            .await
            .context("failed to persist model configuration")?;
        sqlx::query("SELECT pg_notify('oneflow_model_config', 'refresh')")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn create(&self, request: CreateVirtualKey, now: i64) -> Result<CreatedVirtualKey> {
        validate_key_request(&request, now)?;

        let mut id_bytes = [0u8; 16];
        let mut secret_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut id_bytes);
        OsRng.fill_bytes(&mut secret_bytes);
        let id = hex::encode(id_bytes);
        let secret = format!("sk-{id}_{}", hex::encode(secret_bytes));
        let token_sha256 = hex::encode(Sha256::digest(secret.as_bytes()));

        let mut tx = self.pool.begin().await?;
        lock_mutations(&mut tx).await?;
        let config_json: serde_json::Value =
            sqlx::query_scalar("SELECT config FROM virtual_model_config WHERE singleton = TRUE")
                .fetch_one(&mut *tx)
                .await
                .context("failed to read model configuration for allowlist validation")?;
        let config: ModelConfigSet = serde_json::from_value(config_json)
            .context("durable model configuration is invalid")?;
        let configured_models: HashSet<&str> = config
            .models
            .iter()
            .map(|model| model.model_name.as_str())
            .collect();
        if let Some(model) = request
            .allowed_models
            .iter()
            .find(|model| !configured_models.contains(model.as_str()))
        {
            anyhow::bail!("model '{model}' is not currently configured");
        }

        sqlx::query(
            "INSERT INTO virtual_keys \
             (id, name, token_sha256, allowed_models, created_at, expires_at, revoked_at) \
             VALUES ($1, $2, $3, $4, $5, $6, NULL)",
        )
        .bind(&id)
        .bind(&request.name)
        .bind(&token_sha256)
        .bind(serde_json::to_value(&request.allowed_models)?)
        .bind(now)
        .bind(request.expires_at)
        .execute(&mut *tx)
        .await
        .context("failed to persist virtual key")?;
        increment_revision(&mut tx).await?;
        notify_policy_change(&mut tx).await?;
        tx.commit().await?;

        Ok(CreatedVirtualKey {
            key: VirtualKey {
                id,
                name: request.name,
                allowed_models: request.allowed_models,
                created_at: now,
                expires_at: request.expires_at,
                revoked_at: None,
            },
            secret,
        })
    }

    pub async fn get(&self, id: &str) -> Result<Option<VirtualKey>> {
        let row = sqlx::query(
            "SELECT id, name, allowed_models, created_at, expires_at, revoked_at \
             FROM virtual_keys WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to read virtual key")?;
        row.map(row_to_key).transpose()
    }

    pub async fn resolve_hash(&self, identifier: &str) -> Result<Option<String>> {
        let hash = identifier_hash(identifier);
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Ok(None);
        }
        sqlx::query_scalar("SELECT id FROM virtual_keys WHERE token_sha256 = $1")
            .bind(hash)
            .fetch_optional(&self.pool)
            .await
            .context("failed to resolve key identifier")
    }

    pub async fn key_hash(&self, id: &str) -> Result<String> {
        sqlx::query_scalar("SELECT token_sha256 FROM virtual_keys WHERE id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .context("failed to read key identifier")
    }

    pub async fn list_page(
        &self,
        page: i64,
        size: i64,
    ) -> Result<(Vec<(VirtualKey, String)>, i64)> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let total =
            sqlx::query_scalar("SELECT count(*) FROM virtual_keys WHERE revoked_at IS NULL")
                .fetch_one(&mut *tx)
                .await?;
        let rows = sqlx::query(
            "SELECT id, name, token_sha256, allowed_models, created_at, expires_at, revoked_at \
             FROM virtual_keys WHERE revoked_at IS NULL ORDER BY created_at DESC, id DESC LIMIT $1 OFFSET $2"
        ).bind(size).bind((page - 1).checked_mul(size).context("page overflow")?)
            .fetch_all(&mut *tx).await?;
        let keys = rows
            .into_iter()
            .map(|row| {
                let hash = row.try_get("token_sha256")?;
                Ok((row_to_key(row)?, hash))
            })
            .collect::<Result<Vec<_>>>()?;
        tx.commit().await?;
        Ok((keys, total))
    }

    pub async fn delete_keys(&self, identifiers: &[String], now: i64) -> Result<bool> {
        let hashes: Vec<String> = identifiers
            .iter()
            .map(|identifier| identifier_hash(identifier))
            .collect();
        let mut tx = self.pool.begin().await?;
        lock_mutations(&mut tx).await?;
        let found: i64 =
            sqlx::query_scalar("SELECT count(*) FROM virtual_keys WHERE token_sha256 = ANY($1)")
                .bind(&hashes)
                .fetch_one(&mut *tx)
                .await?;
        let unique: HashSet<_> = hashes.iter().collect();
        if found != unique.len() as i64 {
            tx.rollback().await?;
            return Ok(false);
        }
        let affected = sqlx::query(
            "UPDATE virtual_keys SET revoked_at = $1 WHERE token_sha256 = ANY($2) AND revoked_at IS NULL"
        ).bind(now).bind(&hashes).execute(&mut *tx).await?.rows_affected();
        if affected > 0 {
            increment_revision(&mut tx).await?;
            notify_policy_change(&mut tx).await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Read the revision and complete non-revoked policy set from one MVCC
    /// snapshot so every lease has a single durable revision boundary.
    pub async fn snapshot(&self, issued_at: i64) -> Result<VirtualKeyPolicySnapshot> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let revision: i64 =
            sqlx::query_scalar("SELECT revision FROM virtual_key_state WHERE singleton = TRUE")
                .fetch_one(&mut *tx)
                .await
                .context("failed to read virtual-key policy revision")?;
        let rows = sqlx::query(
            "SELECT id, token_sha256, allowed_models, expires_at FROM virtual_keys \
             WHERE revoked_at IS NULL ORDER BY id",
        )
        .fetch_all(&mut *tx)
        .await
        .context("failed to read virtual-key policies")?;
        let keys = rows
            .into_iter()
            .map(|row| {
                let allowed_models: serde_json::Value = row.try_get("allowed_models")?;
                Ok(VirtualKeyPolicy {
                    key_id: row.try_get("id")?,
                    token_sha256: row.try_get("token_sha256")?,
                    allowed_models: serde_json::from_value(allowed_models)?,
                    expires_at: row.try_get("expires_at")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        tx.commit().await?;
        Ok(VirtualKeyPolicySnapshot {
            protocol_version: 1,
            revision,
            issued_at,
            valid_until: issued_at.saturating_add(POLICY_LEASE_SECONDS),
            keys,
        })
    }

    pub async fn listener(&self) -> Result<PgListener> {
        let mut listener = PgListener::connect_with(&self.pool).await?;
        listener.listen("oneflow_virtual_key_policy").await?;
        listener.listen("oneflow_model_config").await?;
        Ok(listener)
    }
}

fn validate_key_request(request: &CreateVirtualKey, now: i64) -> Result<()> {
    if request.name.trim().is_empty() || request.name.chars().count() > 128 {
        anyhow::bail!("name must contain 1 to 128 non-blank characters");
    }
    if request.allowed_models.is_empty() {
        anyhow::bail!("allowed_models must contain at least one model");
    }
    let mut names = HashSet::new();
    for name in &request.allowed_models {
        if name.trim().is_empty() || name.contains('*') {
            anyhow::bail!("allowed_models must contain non-blank exact model names");
        }
        if !names.insert(name) {
            anyhow::bail!("allowed_models must not contain duplicates");
        }
    }
    if let Some(expires_at) = request.expires_at {
        if expires_at < 0 || expires_at <= now {
            anyhow::bail!("expires_at must be a future Unix timestamp or null");
        }
    }
    Ok(())
}

fn row_to_key(row: sqlx::postgres::PgRow) -> Result<VirtualKey> {
    let allowed_models: serde_json::Value = row.try_get("allowed_models")?;
    Ok(VirtualKey {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        allowed_models: serde_json::from_value(allowed_models)?,
        created_at: row.try_get("created_at")?,
        expires_at: row.try_get("expires_at")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}

async fn lock_mutations(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ADVISORY_LOCK_ID)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn increment_revision(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "UPDATE virtual_key_state SET revision = revision + 1 WHERE singleton = TRUE RETURNING revision",
    )
    .fetch_one(&mut **tx)
    .await?)
}

async fn notify_policy_change(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    sqlx::query("SELECT pg_notify('oneflow_virtual_key_policy', 'refresh')")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_request() -> CreateVirtualKey {
        CreateVirtualKey {
            name: "test".to_string(),
            allowed_models: vec!["model-a".to_string()],
            expires_at: None,
        }
    }

    #[test]
    fn validates_allowlists_and_expiry() {
        assert!(validate_key_request(&create_request(), 100).is_ok());
        for models in [
            vec![],
            vec![" ".to_string()],
            vec!["*".to_string()],
            vec!["a".to_string(), "a".to_string()],
        ] {
            let mut request = create_request();
            request.allowed_models = models;
            assert!(validate_key_request(&request, 100).is_err());
        }
        let mut request = create_request();
        request.expires_at = Some(100);
        assert!(validate_key_request(&request, 100).is_err());
        request.expires_at = Some(-1);
        assert!(validate_key_request(&request, 0).is_err());
    }

    #[tokio::test]
    async fn postgres_durability_and_revision_if_configured() {
        let Ok(database_url) = std::env::var("ONEFLOW_TEST_DATABASE_URL") else {
            return;
        };
        let initial = ModelConfigSet {
            version: "integration-v1".to_string(),
            models: vec![protocol::ModelConfig {
                model_name: "integration-model".to_string(),
                provider: "test".to_string(),
                api_base: "https://example.invalid".to_string(),
                parameters: protocol::ModelConfigParameters::new(),
            }],
        };
        let store = VirtualKeyStore::connect(&database_url, &initial)
            .await
            .unwrap();
        let configured_model = store
            .config()
            .await
            .unwrap()
            .models
            .first()
            .expect("integration database must contain a model")
            .model_name
            .clone();
        let mut durable_config = store.config().await.unwrap();
        durable_config.version = format!("integration-{}", uuid::Uuid::new_v4());
        store.set_config(&durable_config).await.unwrap();
        let before = store.snapshot(100).await.unwrap().revision;
        let created = store
            .create(
                CreateVirtualKey {
                    name: format!("integration-{}", uuid::Uuid::new_v4()),
                    allowed_models: vec![configured_model],
                    expires_at: None,
                },
                100,
            )
            .await
            .unwrap();
        assert!(created
            .secret
            .starts_with(&format!("sk-{}_", created.key.id)));
        let snapshot = store.snapshot(101).await.unwrap();
        assert!(snapshot.revision > before);
        assert!(snapshot.keys.iter().any(|key| key.key_id == created.key.id));
        let restarted = VirtualKeyStore::connect(&database_url, &initial)
            .await
            .unwrap();
        assert_eq!(
            restarted.config().await.unwrap().version,
            durable_config.version
        );
        let after_restart = restarted.snapshot(101).await.unwrap();
        assert!(after_restart.revision >= snapshot.revision);
        assert!(after_restart
            .keys
            .iter()
            .any(|key| key.key_id == created.key.id));
        let expected_hash = hex::encode(Sha256::digest(created.secret.as_bytes()));
        assert_eq!(
            snapshot
                .keys
                .iter()
                .find(|key| key.key_id == created.key.id)
                .unwrap()
                .token_sha256,
            expected_hash
        );
        assert!(restarted
            .delete_keys(std::slice::from_ref(&expected_hash), 102)
            .await
            .unwrap());
        assert!(store.delete_keys(&[created.secret], 103).await.unwrap());
        assert_eq!(
            store
                .get(&created.key.id)
                .await
                .unwrap()
                .unwrap()
                .revoked_at,
            Some(102)
        );
        assert!(restarted
            .snapshot(104)
            .await
            .unwrap()
            .keys
            .iter()
            .all(|key| key.key_id != created.key.id));
        assert!(store
            .snapshot(104)
            .await
            .unwrap()
            .keys
            .iter()
            .all(|key| key.key_id != created.key.id));
    }
}
