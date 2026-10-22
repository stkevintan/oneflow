# L2 — Configuration

The control plane and web dev server share a user-created, Git-ignored `config.toml`.
Copy the tracked [`config.example.toml`](../../config.example.toml) before starting
either service. Keep local secrets out of Git and restrict file permissions. The
daemon is a separate executable and uses a standalone config file on its host.

| Consumer | Source | Loader |
| --- | --- | --- |
| Control plane | `config.toml` at the repository root, or `$LLMCP_CONFIG` | `config-provider` crate |
| Web dev and preview servers | Same file | `web/vite.config.ts` (`smol-toml`) |

Model configuration (providers, `api_base`, parameters) is **not** in any of
these files. It is runtime state owned by the control plane and pushed to
daemons; see [protocol.md](./protocol.md).

## Shared `config.toml`

```toml
[control_plane]
bind_addr = "0.0.0.0:4000"                      # control-plane listener
websocket_url = "ws://127.0.0.1:4000/control"   # used by the web /v1 proxy
cors_allowed_origins = []                       # browser origins allowed cross-origin
# Required, unless supplied by environment:
database_url = "postgres://USER:PASSWORD@HOST:5432/DATABASE?sslmode=require"
admin_token = "REPLACE_WITH_RANDOM_ADMIN_TOKEN_AT_LEAST_32_BYTES"
# Provision daemon credentials through the dashboard or POST /v1/daemons.

[web]
addr = "127.0.0.1:5173"                         # Vite dev and preview servers

```

`config-provider` exposes:

```rust
pub struct ServiceConfig {
    pub control_plane: ControlPlaneConfig, // addresses, CORS, database and administrator settings
    pub web: WebConfig,                    // addr
}

ConfigProvider::load()           // $LLMCP_CONFIG or ./config.toml
```

- `deny_unknown_fields` is set, so a typo or stale section is rejected.
- `bind_addr` must parse as a `SocketAddr`; otherwise the control plane exits at
  startup.
- `websocket_url` is the address the dashboard dev and preview proxies use to
  find the control plane and the `cp` URL embedded in newly provisioned daemon
  downloads. Configure a daemon-reachable public WSS URL for remote deployment;
  loopback WS is allowed for local development. Credentials, query strings,
  and fragments are forbidden.
- `cors_allowed_origins` (optional, default `[]`) lists exact browser origins,
  such as `"https://dashboard.example"`, allowed to call `/v1` cross-origin.
  Each entry must be a valid header value, or the control plane exits at
  startup. Empty means no CORS headers are sent; local dev and preview use the
  proxy and need no entry.
- The web bundle itself reads `VITE_CONTROL_PLANE_URL` at build time (see
  [web.md](./web.md#base-url)) when it is hosted on another origin.
- `database_url` and `admin_token` are mandatory.
  `ONEFLOW_DATABASE_URL` and `ONEFLOW_ADMIN_TOKEN`
  override their respective TOML settings. After resolution both settings
  are required; missing either or both causes startup to fail.
  Empty or invalid credentials are rejected, not replaced by file defaults.
  PostgreSQL migrations run on startup. See [deployment](../virtual-keys.md).
- Daemon credentials are provisioned and managed through the dashboard/API and
  persisted as hashes in PostgreSQL. Static `daemon_tokens` and
  `ONEFLOW_DAEMON_TOKENS` are rejected; remove them and provision new tokens.

## Daemon configuration

### CLI

| Flag | Required | Meaning |
| --- | --- | --- |
| `--cp <URL>` | no | Control-plane WebSocket URL; overrides environment and TOML |
| `-c, --config <PATH>` | no | Select a daemon TOML file instead of `./config.toml` |
| `--listen-addr <SOCKET>` | no | HTTP listener address |
| `--daemon-id <ID>` | no | Identity reported during registration |
| `--connect-timeout-ms <MS>` | no | Upstream connection timeout |
| `--read-timeout-ms <MS>` | no | Upstream per-read timeout |
| `--request-timeout-ms <MS>` | no | Upstream request timeout |
| `--max-response-bytes <BYTES>` | no | Maximum buffered upstream response |

### Daemon TOML (`config.toml`)

The daemon looks for a standalone `config.toml` in its current working
directory. Its config is independent of the control-plane/web configuration.
The example file can be copied to the remote host as `config.toml`, or selected
with `--config`. Settings use top-level keys; see
[`daemon/config.example.toml`](../../daemon/config.example.toml).

| Key | Default | Meaning |
| --- | --- | --- |
| `cp` | — | Control-plane WebSocket URL (required unless supplied by CLI or environment) |
| `control_token` | — | Required provisioned daemon token unless `ONEFLOW_DAEMON_TOKEN` is set; included in dashboard downloads |
| `listen_addr` | `127.0.0.1:9000` | Local-only HTTP listener; inference routes use fixed paths including `/v1/chat/completions` |
| `daemon_id` | `daemon-poc-001` | Identity reported in registration and telemetry |
| `connect_timeout_ms` | 5000 | Upstream TCP/TLS connect timeout |
| `read_timeout_ms` | 60000 | Upstream per-read timeout (bounds streaming stalls) |
| `request_timeout_ms` | 120000 | Upstream total timeout for non-streaming requests |
| `max_response_bytes` | 16777216 | Cap on a buffered non-streaming response body |

TOML keys may be supplied through the documented overrides; unknown keys are
rejected. A control token from the file or `ONEFLOW_DAEMON_TOKEN` is required. There is no authentication-mode
setting: virtual keys and policy leases are always enforced.

### Environment variables

Deployments may override the control-plane TOML settings with
`ONEFLOW_DATABASE_URL` and `ONEFLOW_ADMIN_TOKEN`, and uses
`ONEFLOW_DAEMON_TOKEN` on each daemon. See
[deployment and usage](../virtual-keys.md). Keep all secrets out of tracked files.

Resolution order: **CLI option → environment variable → TOML → built-in
default**. The control-plane URL is required and can be supplied by `--cp`,
`CONTROL_PLANE_URL`, or the TOML `cp` setting.

| Variable | Overrides / purpose |
| --- | --- |
| `CONTROL_PLANE_URL` | Control-plane WebSocket URL |
| `DAEMON_LISTEN_ADDR` | `listen_addr` |
| `DAEMON_ID` | `daemon_id` |
| `UPSTREAM_CONNECT_TIMEOUT_MS` | `connect_timeout_ms` |
| `UPSTREAM_READ_TIMEOUT_MS` | `read_timeout_ms` |
| `UPSTREAM_REQUEST_TIMEOUT_MS` | `request_timeout_ms` |
| `MAX_UPSTREAM_RESPONSE_BYTES` | `max_response_bytes` |
| `ONEFLOW_DAEMON_TOKEN` | Overrides TOML `control_token`, including downloaded credentials |
| `CLUSTER_NAME` | Cluster label in registration (default `cluster-ssh198`) |
| `HOSTNAME` | Hostname in registration (default `unknown`) |
| `RUST_LOG` | Log filter (default `oneflowd=info`) |
| *any name in `api_key_env`* | Upstream provider key, referenced from model parameters |

## Known gaps

- No hot reload: both services read their files only at startup.
- Model configuration and credentials are durable; presence and recent usage
  remain process-local.
- Native TLS termination is not implemented; remote deployments
  require an HTTPS/WSS reverse proxy.
