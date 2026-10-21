# Standalone daemon

The daemon is built as an independent executable and does not load the
control-plane/web configuration or depend on the `config-provider` crate.
Build it from the repository root:

```sh
cargo build --release -p daemon
```

Copy `target/release/daemon` and a daemon-specific TOML file to the daemon
machine, then start it with the control plane's reachable WebSocket URL:

```sh
./daemon --cp ws://control-plane.example:4000/control --config ./daemon.toml
```

`--cp` is the endpoint the daemon connects to; the daemon always initiates this
WebSocket connection. `--config` is the path to a local TOML file. Start from
[`config.example.toml`](./config.example.toml). The daemon API defaults to
`0.0.0.0:0`, so the OS chooses an available port and the selected address is
logged at startup. Set `listen_addr` to a fixed socket address if required.

`DAEMON_API_KEYS` optionally sets comma-separated client API keys. The
configuration file also supports `daemon_id`, `connect_timeout_ms`,
`read_timeout_ms`, `request_timeout_ms`, and `max_response_bytes`.
`DAEMON_ID`, `UPSTREAM_CONNECT_TIMEOUT_MS`, `UPSTREAM_READ_TIMEOUT_MS`,
`UPSTREAM_REQUEST_TIMEOUT_MS`, and `MAX_UPSTREAM_RESPONSE_BYTES` override the
matching TOML values. `CLUSTER_NAME` configures the cluster label.
