# Standalone daemon

The daemon is built as an independent executable and does not load the
control-plane/web configuration or depend on the `config-provider` crate.
Build it from the repository root:

```sh
cargo build --release -p oneflowd
```

Copy `target/release/oneflowd` and its standalone `config.toml` to the remote
machine, then start it from that config's directory:

```sh
./oneflowd
```

The daemon automatically reads `./config.toml` when present. The daemon's
configuration is independent of the control-plane/web configuration. Start
from [`config.example.toml`](./config.example.toml), which uses top-level
daemon settings, and place it on the remote machine as `config.toml`. Use
`--config PATH` to select a different file, or pass settings directly as CLI
options. CLI options take precedence over environment variables, which take
precedence over TOML values and then built-in defaults. The control-plane URL
must be provided with `--cp`, `CONTROL_PLANE_URL`, or the `cp` TOML setting;
the daemon always initiates this WebSocket connection.

The repository-root `config.toml` is for the control plane and web server, not
the daemon. When running from the repository root, specify the daemon file
explicitly, for example `--config daemon/config.example.toml`.

The API listens on `127.0.0.1:0` by default, so it is available only to local
clients and the OS chooses an available port. The selected address is logged
at startup. See [`config.example.toml`](./config.example.toml) for the
available daemon settings and their defaults.

`DAEMON_API_KEYS` optionally sets comma-separated client API keys.
`DAEMON_ID`, `DAEMON_LISTEN_ADDR`, `UPSTREAM_CONNECT_TIMEOUT_MS`,
`UPSTREAM_READ_TIMEOUT_MS`, `UPSTREAM_REQUEST_TIMEOUT_MS`, and
`MAX_UPSTREAM_RESPONSE_BYTES` override matching TOML values. CLI options
override those environment variables. `CLUSTER_NAME` configures the cluster
label.
