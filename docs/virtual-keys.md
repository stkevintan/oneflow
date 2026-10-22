# Virtual-key deployment and usage

Virtual keys let external OpenAI-compatible apps use selected model aliases
without receiving provider credentials. Budgets and rate limits are not yet
implemented.

## 1. Start an authenticated control plane

Provision PostgreSQL and a dedicated database. Create your local configuration:

```sh
cp config.example.toml config.toml
chmod 600 config.toml
```

The local `config.toml` is Git-ignored; only `config.example.toml` is committed.
Under `[control_plane]`, set (values below are placeholders):

```toml
database_url = "postgres://USER:PASSWORD@HOST:5432/DATABASE?sslmode=require"
admin_token = "REPLACE_WITH_RANDOM_ADMIN_TOKEN_AT_LEAST_32_BYTES"
```

Use a PostgreSQL URL, percent-encoding special characters in usernames and
passwords. Select SSL settings appropriate for your deployment; `sslmode=disable`
is only suitable for a trusted local network. Then run `cargo run -p control-plane`.
`LLMCP_CONFIG` can select a different configuration file.

Alternatively, supply environment variables. Each overrides its matching TOML
setting, so secrets can stay in your deployment's secret manager:

```sh
export ONEFLOW_DATABASE_URL='postgresql://USER:PASSWORD@HOST/DATABASE'
export ONEFLOW_ADMIN_TOKEN='REPLACE_WITH_RANDOM_ADMIN_TOKEN'
cargo run -p control-plane
```

Use a cryptographically random administrator credential. Daemon credentials
are generated through the API/dashboard, not configuration.
Both control-plane settings must resolve from TOML and/or environment; a partial setup
is rejected rather than silently reverting to legacy mode.
The database schema is initialized at startup. Model configuration and virtual
keys are durable in this mode; daemon presence and recent usage remain
process-local and bounded.

Expose management and `/control` only through an HTTPS/WSS reverse proxy for
remote access, and prevent direct access to its plaintext backend. Never place
bearer credentials in URLs or logs, or any secrets in committed configuration or a frontend build-time
environment variable. Local development may use loopback HTTP/WS.

The dashboard has an **Administrator token** input. It retains the applied
token in memory only; reload clears it. Key lifecycle operations are currently
available through the API rather than a dedicated dashboard editor.

## 2. Configure models

Send the usual full model configuration with the administrator token:

```sh
curl --fail-with-body http://127.0.0.1:4000/v1/config \
  -X PUT \
  -H "Authorization: Bearer $ONEFLOW_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"version":"v1","models":[{"model_name":"my-model","provider":"custom_openai","api_base":"https://provider.example/v1","parameters":{"api_key_env":"MY_PROVIDER_API_KEY"}}]}'
```

Model permissions apply to public `model_name` aliases before any
`upstream_model` rewrite. Reusing an alias for a different provider also
changes the destination for keys granted that alias.

## 3. Start a virtual-key daemon

In the dashboard, apply the administrator token, then use **Daemon credentials**
to create a unique daemon ID. Save the generated token immediately; it is shown
once, kept only in page memory, and cannot be recovered. Dismiss the token after
deploying it. The same operation is available through the API:

```sh
curl --fail-with-body http://127.0.0.1:4000/v1/daemons \
  -X POST -H "Authorization: Bearer $ONEFLOW_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"daemon_id":"daemon-poc-001"}'
```

The 201 response contains `{ "credential": { "daemon_id": "...", ... }, "token": "ofd_..." }`.
Only the SHA-256 token hash is persisted in PostgreSQL. A duplicate ID returns
409, even for a revoked daemon. If a creation response is lost, rotate that ID
instead of trying to recover its token.

Add this to the daemon's standalone TOML (not the repository-root config):

```toml
cp = "ws://127.0.0.1:4000/control"
daemon_id = "daemon-poc-001"
listen_addr = "127.0.0.1:9000"
auth_mode = "virtual_keys"
```

On the daemon host, set the matching control credential and provider credential:

```sh
export ONEFLOW_DAEMON_TOKEN='REPLACE_WITH_TOKEN_FROM_PROVISIONING'
export MY_PROVIDER_API_KEY='REPLACE_WITH_PROVIDER_KEY'
oneflowd -c /path/to/daemon.toml
```

Remote control connections must use `wss://`. Virtual-key mode refuses static
`DAEMON_API_KEYS` and never falls back to anonymous access. The CLI override
is `--auth-mode virtual_keys`. The default remains `legacy`.

### Manage daemon credentials

The dashboard lists provisioned credentials even when the daemon is offline,
separately from the connected-daemon table. **Rotate** replaces the token;
**Revoke** disables the credential. Both require confirmation. Rotating a
revoked daemon reactivates it. Deploy each replacement token and restart the
daemon; tokens are not automatically installed on daemon hosts.

| Endpoint | Purpose |
| --- | --- |
| `GET /v1/daemons/credentials` | List metadata only, including revoked credentials |
| `POST /v1/daemons/{daemon_id}/rotate` | Issue a replacement token once |
| `POST /v1/daemons/{daemon_id}/revoke` | Revoke idempotently and retain metadata |

All operations require the administrator token and PostgreSQL-backed mode.
Credential responses use `Cache-Control: no-store`. They never return stored
hashes, and only creation/rotation return plaintext tokens.

New connections validate credentials against PostgreSQL. Existing connections
revalidate before processing messages/sending updates and every 5 seconds;
revocation/rotation fences them across control-plane instances. In-flight
messages may finish. Database validation failures close the control channel.
Disconnected daemons can still serve their existing cached inference permissions
until the policy lease expires (at most 300 seconds plus clock skew). Revoke
client virtual keys separately if their access should also be removed.

**Migration:** static `control_plane.daemon_tokens` and `ONEFLOW_DAEMON_TOKENS`
are no longer supported and fail startup rather than silently being ignored.
Remove them, provision the old daemon IDs through this API, and deploy the new
tokens to daemon hosts.

## 4. Issue a key for the app

```sh
curl --fail-with-body http://127.0.0.1:4000/key/generate \
  -H "Authorization: Bearer $ONEFLOW_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"key_alias":"external-app","models":["my-model"],"duration":"30d"}'
```

The 200 response contains `{ "key": "sk-...", "token": "<sha256>", ...metadata }`.
Save the secret securely: it is returned once and cannot be recovered.
`duration` is a positive integer followed by `s`, `m`, `h`, or `d`; omitted or
null means no expiry. Responses use `expires` as an RFC 3339 UTC timestamp.
All allowlist names must exist when creating the key; wildcards and empty lists
are rejected.
Creation does not guarantee that every daemon has received its policy yet.

Configure the external app:

- Base URL: `http://127.0.0.1:9000/v1`.
- API key: the newly returned `key`.
- Model: `my-model`.

Use HTTPS and appropriate firewall rules if clients connect from another host.
A virtual key must never be used as an administrator or provider credential.

## 5. List, inspect, and revoke

```sh
curl --fail-with-body 'http://127.0.0.1:4000/key/list?page=1&size=10&return_full_object=true' \
  -H "Authorization: Bearer $ONEFLOW_ADMIN_TOKEN"

curl --fail-with-body "http://127.0.0.1:4000/key/info?key=$KEY_HASH" \
  -H "Authorization: Bearer $ONEFLOW_ADMIN_TOKEN"

curl --fail-with-body http://127.0.0.1:4000/key/delete \
  -X POST -H "Authorization: Bearer $ONEFLOW_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d "{\"keys\":[\"$KEY_HASH\"]}"
```

`KEY_HASH` is the `token` hash returned during generation or listing, not
the internal `token_id`. Metadata responses never return a bearer secret.
Deletion is permanent and idempotent. Rotate by creating a new key, updating the client,
then deleting the previous one. Deleted metadata remains available through
`/key/info`, but is omitted from `/key/list`. Batch deletion is atomic:
an unknown identifier returns 404 without deleting any supplied key.
A lost creation response requires finding the orphaned key's hash through
listing and deleting it before creating another; do not blindly retry.

## LiteLLM compatibility and migration

The `/key/*` API implements the core generate/info/list/delete shapes from
[LiteLLM virtual keys](https://docs.litellm.ai/docs/proxy/virtual_keys). It is
a supported subset, not a drop-in replacement for every LiteLLM management API:

- `key_alias`, `models`, and optional `duration` are accepted on generation.
  Explicit non-empty model lists are required; unlike LiteLLM, empty or
  omitted models never grant all-model access.
- `/key/list` uses page/size pagination and returns hashes by default, or
  metadata objects with `return_full_object=true`. Expired non-deleted keys
  remain listed. Unsupported filters are rejected.
- Info/delete accept hashes or raw keys, but always return hashes rather than
  echoing raw keys. Prefer hashes to avoid secrets in URLs and access logs.
- User/team ownership, metadata, budgets, rate limits, alias-based deletion,
  and `/key/update` are unsupported. Unsupported request fields return 400
  instead of silently ignoring policy settings.
- Administrator auth is required for all management operations; inference
  keys cannot manage themselves or other keys. Error responses retain
  Oneflow's `{ "error": "message" }` shape.

This replaces the former `/v1/virtual-keys` API; old routes return 404.
Migrate `name` to `key_alias`, `allowed_models` to `models`, absolute
`expires_at` to relative `duration`, and response `secret` to `key`.
Use SHA-256 identifiers from `/key/list` instead of internal IDs for
management. Previously issued `ofk_...` keys still work for inference and
lookup/deletion; new keys start with `sk-`. Policy hashing and key IDs in
telemetry are unchanged.

## Policy health and offline behavior

Daemons receive full policy snapshots refreshed every 60 seconds. A snapshot
is valid for at most 300 seconds. Disconnected daemons may serve cached keys
only until that deadline, then return 503 (`auth_policy_unavailable`).
Startup also fails closed until the first snapshot. Keep host clocks synchronized.

Revocation success means committed to PostgreSQL, not acknowledged by every
daemon. Check `/v1/daemons` for `auth_policy_revision` and
`auth_policy_valid_until`; these are last acknowledgements, not a guarantee of
continued connectivity. Revocation lag is bounded by the policy lease plus
up to 30 seconds of clock skew. Already admitted requests/streams may finish.

Rejected credentials return 401, denied model access returns 403, and a missing
referenced provider credential returns 503. `/v1/models` is filtered to granted
models. Usage events carry `virtual_key_id`, never the bearer secret.

Without virtual-key environment settings the control plane remains in legacy
mode. Do not treat that unauthenticated mode as a production management API.
See [the detailed design](./L2-detailed-design/virtual-keys.md) and
[the API contract](../contract/openapi.yaml) for exact semantics.
