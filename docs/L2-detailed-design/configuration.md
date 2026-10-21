# L2 — Configuration

The control plane and web dev server share the repository's `config.toml`. The
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

[web]
addr = "127.0.0.1:5173"                         # Vite dev and preview servers

```

`config-provider` exposes:

```rust
pub struct ServiceConfig {
    pub control_plane: ControlPlaneConfig, // bind_addr, websocket_url, cors_allowed_origins
    pub web: WebConfig,                    // addr
}

ConfigProvider::load()           // $LLMCP_CONFIG or ./config.toml
```

- `deny_unknown_fields` is set, so a typo or stale section is rejected.
- `bind_addr` must parse as a `SocketAddr`; otherwise the control plane exits at
  startup.
- `websocket_url` is the address the dashboard dev and preview proxies use to
  find the control plane. Daemons use their own config.
- `cors_allowed_origins` (optional, default `[]`) lists exact browser origins,
  such as `"https://dashboard.example"`, allowed to call `/v1` cross-origin.
  Each entry must be a valid header value, or the control plane exits at
  startup. Empty means no CORS headers are sent; local dev and preview use the
  proxy and need no entry.
- The web bundle itself reads `VITE_CONTROL_PLANE_URL` at build time (see
  [web.md](./web.md#base-url)) when it is hosted on another origin.

## Daemon configuration

### CLI

| Flag | Required | Meaning |
| --- | --- | --- |
| `--cp <URL>` | no | Control-plane WebSocket URL; overrides environment and TOML |
| `--config <PATH>` | no | Select a daemon TOML file instead of `./config.toml` |
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
| `listen_addr` | `127.0.0.1:0` | Local-only HTTP listener. Port 0 means the OS selects a port, which is logged at startup |
| `daemon_id` | `daemon-poc-001` | Identity reported in registration and telemetry |
| `connect_timeout_ms` | 5000 | Upstream TCP/TLS connect timeout |
| `read_timeout_ms` | 60000 | Upstream per-read timeout (bounds streaming stalls) |
| `request_timeout_ms` | 120000 | Upstream total timeout for non-streaming requests |
| `max_response_bytes` | 16777216 | Cap on a buffered non-streaming response body |

All keys are optional; unknown keys are rejected.

### Environment variables

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
| `DAEMON_API_KEYS` | Comma-separated client keys for `/v1/*`. Unset means open |
| `CLUSTER_NAME` | Cluster label in registration (default `cluster-ssh198`) |
| `HOSTNAME` | Hostname in registration (default `unknown`) |
| `RUST_LOG` | Log filter (default `daemon=info`) |
| *any name in `api_key_env`* | Upstream provider key, referenced from model parameters |

## Known gaps

- No hot reload: both services read their files only at startup.
- The control plane's model configuration is not loaded from a file, so it
  resets to the built-in default on restart.
- TLS and authentication settings do not exist yet.
