# Standalone daemon

The daemon is built as an independent executable and does not load the
control-plane/web configuration or depend on the `config-provider` crate.
Build it from the repository root:

```sh
cargo build --release -p oneflowd
```

Create a daemon in the dashboard and click **Download config.toml**. It includes
the control-plane URL, daemon ID, and one-time token. Copy
`target/release/oneflowd` and the downloaded file securely to the remote
machine, then start it from that config's directory:

```sh
chmod 600 config.toml
./oneflowd
```

The daemon automatically reads `./config.toml` when present. The daemon's
configuration is independent of the control-plane/web configuration. Start
from [`config.example.toml`](./config.example.toml), which uses top-level
daemon settings, and place it on the remote machine as `config.toml`. Use
`--config PATH` or `-c PATH` to select a different file, or pass settings
directly as CLI options. CLI options take precedence over environment variables, which take
precedence over TOML values and then built-in defaults. The control-plane URL
must be provided with `--cp`, `CONTROL_PLANE_URL`, or the `cp` TOML setting;
the daemon always initiates this WebSocket connection.

The repository-root `config.toml` is for the control plane and web server, not
the daemon. When running from the repository root, specify the daemon file
explicitly, for example `--config daemon/config.example.toml`.

The API listens on `127.0.0.1:9000` by default, so it is available only to
local clients at a stable address. The fixed API paths include
`/v1/chat/completions`, `/v1/models`, and `/healthz`. Override the listener
with `--listen-addr`, `DAEMON_LISTEN_ADDR`, or `listen_addr` in the config.
See [`config.example.toml`](./config.example.toml) for the available daemon
settings and their defaults.

Logs are written to standard error (usually visible in the terminal where
`oneflowd` is running, or through the service manager when run as a service).
Startup logs include the daemon ID, control-plane URL, and bound listener
address. Connection-state logs show connecting, connected, registered, and
reconnecting states. Set `RUST_LOG=oneflowd=debug` for more detail.

`DAEMON_ID`, `DAEMON_LISTEN_ADDR`, `UPSTREAM_CONNECT_TIMEOUT_MS`,
`UPSTREAM_READ_TIMEOUT_MS`, `UPSTREAM_REQUEST_TIMEOUT_MS`, and
`MAX_UPSTREAM_RESPONSE_BYTES` override matching TOML values. CLI options
override those environment variables. `CLUSTER_NAME` configures the cluster
label.

## Managed virtual keys

Virtual API keys are mandatory. The downloaded TOML's `control_token` is the daemon's
control-channel credential; `ONEFLOW_DAEMON_TOKEN` overrides it if set.
Provision it through **Daemon credentials** in the
dashboard or administrator-authenticated `POST /v1/daemons`; configure the same
`daemon_id` on the daemon. The token is returned once. Rotation replaces it;
download/deploy the replacement configuration and restart the daemon. There is no authentication mode
switch and no static client-key or anonymous-access path.
Remote control connections require `wss://`; local development may use
loopback `ws://`.

The daemon rejects protected requests until it receives a valid policy snapshot.
Policies grant exact model aliases and expire after at most five minutes
without a refresh, including while disconnected. Upstream credentials remain
separate from virtual keys; client virtual keys are never forwarded upstream.
See [deployment and API examples](../docs/virtual-keys.md).
