# LLM Control Plane

A Rust-based control plane and standalone inference gateway for centrally
managing model configuration across clusters. Operators update configuration
through a separate web dashboard; daemons connect outbound to the control plane,
serve OpenAI-compatible requests locally, and report usage and health telemetry.

Inference traffic flows directly from each daemon to its configured model
provider. It does not pass through the control plane.

> **Proof of concept:** The current control plane keeps configuration, daemon
> presence, and usage history in memory, so this state is lost when it restarts.
> The management API declares bearer authentication in its contract but does
> not enforce it yet. See [known gaps](docs/L1-system-design.md#known-gaps) and
> [ADR 0001](docs/adr/0001-api-only-stateless-control-plane.md).

## Architecture

```text
Operator ──> Web dashboard ──HTTP/JSON──> Control plane
                                            ▲       │
                           outbound WebSocket       │ config + telemetry
                                            │       ▼
Client app ──OpenAI-compatible HTTP──> Daemon ──HTTP(S)──> Model provider
```

- **Control plane** — API-only Rust service exposing the management API at
  `/v1/*` and the daemon control channel at `/control`.
- **Daemon** — standalone Rust executable that connects to the control plane,
  serves OpenAI-compatible inference endpoints, and calls upstream providers.
- **Web dashboard** — separately deployed React/TypeScript client for daemon
  status, usage telemetry, and model configuration.
- **Shared contract** — `contract/openapi.yaml` defines the management API and
  generates Rust DTOs and TypeScript types.

## Quick start

Requirements: Rust toolchain and Node.js/npm.

1. Start the control plane from the repository root:

   ```sh
   cargo run -p control-plane
   ```

   By default it listens on `0.0.0.0:4000`, using [`config.toml`](config.toml).

2. In another terminal, install dashboard dependencies and start the web app:

   ```sh
   cd web
   npm install
   npm run dev
   ```

   Open the address printed by Vite (defaults to `http://127.0.0.1:5173`).
   The dev server proxies `/v1` requests to the control plane.

3. Configure a model in the dashboard, then start the daemon from the
   repository root with its standalone example config:

   ```sh
   cargo run -p daemon -- --config daemon/config.example.toml
   ```

   The daemon is a separate binary and reads its own top-level settings from
   `config.toml` in its working directory, or from `--config PATH`. CLI options
   override environment variables, which override TOML values. For example,
   `cargo run -p daemon -- --cp ws://127.0.0.1:4000/control --listen-addr 127.0.0.1:9000`.
   The API listens on `127.0.0.1` at an OS-selected port by default and logs
   the selected address. Configure the model's provider URL and credentials in
   the model configuration; the daemon supports environment-backed provider
   keys via `api_key_env`.

For deployment guidance and configuration options, see the
[daemon guide](daemon/README.md) and
[configuration design](docs/L2-detailed-design/configuration.md).

## Build and test

```sh
cargo build --workspace
cargo test --workspace

cd web
npm test
npm run typecheck
npm run build
```

To regenerate API bindings after changing the OpenAPI contract, run
[`scripts/generate-types.sh`](scripts/generate-types.sh). Its `--check` mode
checks that generated output is current:

```sh
scripts/generate-types.sh --check
```

The Rust generator CLI is required for this script; see its setup comment.

## Repository layout

| Path | Description |
| --- | --- |
| [`control-plane/`](control-plane/) | Management API and WebSocket service |
| [`daemon/`](daemon/) | Standalone inference gateway |
| [`protocol/`](protocol/) | Shared protocol messages and generated Rust DTOs |
| [`config-provider/`](config-provider/) | Shared service configuration loader |
| [`contract/`](contract/) | OpenAPI source of truth |
| [`web/`](web/) | Operator dashboard |
| [`docs/`](docs/README.md) | Architecture, detailed design, and ADRs |

## Documentation

Start with the [architecture documentation index](docs/README.md). It links
the system context, deployment and runtime flows, component design, and
architecture decision records.
