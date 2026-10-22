# Agent Development Log - llm-control-plane-monorepo

## Goal
Create a **running Proof of Concept** for a new centralized control system:
- One central control plane that manages model providers, configuration, virtual keys, spend tracking, and admin UI.
- Multiple lightweight **daemons** (drop-in replacements for LiteLLM) deployed on 3 different clusters (including the complex "ssh 198"/CarHer Pro instance).
- Persistent bidirectional control channel from each daemon to the central plane.
- **No inference traffic** goes through the central control plane.
- Must be resilient across different network environments (Hangzhou + Singapore).

**Final Technical Decision**: Use **Rust + WebSocket + JSON** (instead of gRPC) for better firewall and network compatibility.

## Architecture

Full architecture docs (L0 context → L1 containers → L2 detailed design) live in
[`docs/`](docs/README.md).

`contract/openapi.yaml` is the single source of truth for the JSON API. Both language
bindings are generated from it; the WebSocket control channel is hand-written but
reuses the generated model types.

```
contract/openapi.yaml                 source of truth, edited first
        |
        +-- openapi-typescript --------> web/src/api/schema.d.ts   (generated)
        |                                       |
        |                                       v
        |                               web/ React 19 + TS + Tailwind 4 + Vite
        |                                       |  npm run build -> web/dist/ (static,
        |                                       |  deployed separately; calls /v1 only)
        |
        +-- openapi-to-rust ------------> protocol/src/generated/  (generated DTOs)
                                                |
                                                v
                                        protocol/  (generated DTOs + hand-written
                                                    WebSocket control messages,
                                                    which reuse generated ModelConfig)
                                                |
                        +-----------------------+-----------------------+
                        v                                               v
        control-plane binary                                   daemon binary
        /control (WS), /v1/* (JSON API only)                   OpenAI-compatible /v1/*
                        ^                                               |
                        |   WebSocket /control (JSON)                  v
                        +-------------------------------- upstream providers (DeepSeek, vLLM)
                                          inference never touches the control plane
```

**Contract-first rules**
1. `contract/openapi.yaml` changes first, and is reviewed like code.
2. Regenerate both bindings: `scripts/generate-types.sh`.
   - TypeScript: `openapi-typescript` → `web/src/api/schema.d.ts`
   - Rust DTOs: `openapi-to-rust` → `protocol/src/generated/` (config: `openapi-to-rust.toml`)
3. Generated files are never hand-edited. `scripts/generate-types.sh --check` fails when
   committed output is stale (use it in CI).
4. Rust HTTP handlers are hand-written, but `cargo test` runs `contract_tests`
   (`control-plane/src/api.rs`), which parses the contract and asserts that every
   declared path/method exists on the axum router, that every GET response carries every
   contract-required field (following `$ref`s), and that unknown paths and methods return
   JSON 404/405.
5. Rust DTOs and handlers are intentionally decoupled: the DTOs are generated, the
   handlers are not. `#[rustfmt::skip]` keeps `rustfmt` off `protocol/src/generated/` so
   the generator's `--check` stays meaningful.

## What Has Been Done
- Created clean monorepo at `/home/cltx/Projects/llm-control-plane-monorepo`
- Implemented **Contract** (`contract/openapi.yaml`): OpenAPI 3.1 description of the control plane JSON API
- Implemented **`protocol`** crate: generated DTOs (`src/generated/`) plus the hand-written
  WebSocket control messages (`ControlMessage`, `DaemonMessage`, `Registration`,
  `Heartbeat`, `Telemetry`). Protocol messages reuse the generated `ModelConfig`, so the
  model definition exists exactly once. (This crate was previously named `proto`.)
- Implemented **Control Plane** (`control-plane`):
  - WebSocket endpoint at `/control` for the daemon control channel
  - Tracks every connection: registration, heartbeats, telemetry totals, last-seen
  - Broadcasts configuration updates to all connected daemons via a `tokio::sync::broadcast` channel
  - Pure API service (ADR 0001 step 1): `/v1/health`, `/v1/status`, `GET/PUT /v1/config`,
    `/v1/daemons[/{connection_id}]`, paginated `/v1/usage/events`, using the generated DTOs.
    Serves no HTML; every error is a JSON `ErrorResponse`. Optional CORS allow-list.
  - Bearer auth is declared in the contract but **not enforced** yet
  - Modules: `api.rs` (routes), `control.rs` (WebSocket), `store.rs` (`MemoryStore`, the
    seam for PostgreSQL)
- Implemented **Daemon** (`daemon`), split into `main.rs` (wiring/state/control channel),
  `api.rs` (routes/auth) and `upstream.rs` (usage accounting/streaming):
  - Connects to control plane via WebSocket and registers with cluster name (`cluster-ssh198`)
  - Applies pushed model configuration and sends periodic heartbeats plus telemetry
  - Serves `/v1/models`, `/v1/models/{id}`, `/v1/chat/completions` and `/healthz`
  - Proxies inference directly to configured upstream providers
  - **Real token accounting**: provider-reported usage is read from unary bodies and from
    the terminal SSE frame; `stream_options.include_usage` is injected for OpenAI-shaped
    providers (opt out per model with `include_usage = "false"`); a provider that reports no
    usage is flagged `usage_reported: false` rather than recorded as zero
  - Telemetry is emitted **after** the response is consumed, via a guard that also fires on
    client disconnect, and reports `latency_ms`, `status_code`, `streamed` and `request_id`
  - Timeouts (`connect`/`read`/`request`), a capped unary response buffer, and a bounded SSE
    scan buffer
  - Optional client authentication via `DAEMON_API_KEYS` (401 in OpenAI error shape)
  - In-flight request accounting that stays accurate for the lifetime of a stream
- Implemented **Dashboard** (`web/`): React 19 + TypeScript + Tailwind 4 + Vite; a standalone `/v1`
  client (dev/preview proxy, or `VITE_CONTROL_PLANE_URL` for another origin)
  - Connected daemons table, recent telemetry, model configuration editor that pushes to all daemons
  - Vitest + jsdom component tests (`npm test`)
- Added `scripts/generate-types.sh` to regenerate both bindings, with a `--check` mode

## Current Status
- Control Plane listens on `control_plane.bind_addr` (default `0.0.0.0:4000`) and serves only
  `/control` (WebSocket) and `/v1/*` (JSON); `/` is a JSON 404
- Dashboard runs separately via `npm run dev` / `npm run preview` on `web.addr` (default `127.0.0.1:5173`)
- Daemon serves an OpenAI-compatible API on its `listen_addr` and reacts to pushed configuration
- **Phase 1 complete**: the daemon inference endpoint reports real usage. Verified end-to-end
  against a mock upstream: a unary call reported 17/5 tokens, a streaming call reported 21/13
  tokens with the usage frame deliberately split across two socket writes, an upstream that
  omits `usage` was recorded as `usage_reported: false` with zero tokens rather than a silent
  zero, unauthenticated calls were rejected with 401 before reaching the upstream, and
  `/healthz` reported zero dropped telemetry
- Earlier end-to-end checks still hold: config pushed through `PUT /v1/config` is broadcast, a live
  daemon immediately serves the new model list, `upstream_model` rewrites the model id sent
  upstream, and `api_key_env` injects the upstream bearer token
- `cargo test --workspace` passes: 13 control-plane (9 contract + 4 store), 15 daemon, and 3
  config-provider tests; `openapi-to-rust --check`, `npm test`, `npm run typecheck` and
  `npm run build` pass. Smoke-tested end to end: control plane + daemon + `vite preview`, config
  PUT reached the daemon, and the resulting usage event was listed by `/v1/usage/events`
- POC demonstrates centralized config distribution without sending inference traffic through the control plane

## Build & Run

```sh
# One-time: the Rust contract generator
cargo install --locked openapi-to-rust

# Regenerate bindings after editing contract/openapi.yaml
scripts/generate-types.sh

# Control plane (API only)
cargo run -p control-plane                 # http://localhost:4000/v1/status

# Dashboard (separate client; proxies /v1 to the control plane)
cd web && npm install && npm run dev       # http://127.0.0.1:5173/
cd web && npm run build && npm run preview # production bundle, same address

# Daemon
cargo build --release -p oneflowd         # target/release/oneflowd
target/release/oneflowd --config daemon/config.example.toml

# Checks
cargo test --workspace                     # contract drift guard
cargo fmt --all -- --check
scripts/generate-types.sh --check          # committed bindings are current
cd web && npm test && npm run typecheck
```

The Rust build does not depend on the frontend: `web/dist/` is git-ignored build output.

### Daemon configuration

The daemon is a standalone binary with its own TOML config, separate from the
control-plane/web config. It reads `./config.toml` when present, or a file
selected by `--config`. CLI options override environment variables, which
override TOML settings and built-in defaults. The control-plane URL is required
and can be set with `--cp`, `CONTROL_PLANE_URL`, or the TOML `cp` setting. Its
API listener defaults to `127.0.0.1:0`, so it is local-only; the OS selects an
available port and the daemon logs the chosen address.
`DAEMON_API_KEYS` optionally supplies comma-separated client keys.

| Variable | Purpose |
| --- | --- |
| `DAEMON_ID` | Overrides the identity in TOML |
| `DAEMON_API_KEYS` | Comma-separated client keys for `/v1/*`; unset means the API is **open** |
| `CLUSTER_NAME` | Cluster label in the dashboard |
| `UPSTREAM_CONNECT_TIMEOUT_MS` | Overrides the TOML upstream connect timeout |
| `UPSTREAM_READ_TIMEOUT_MS` | Overrides the TOML upstream read timeout |
| `UPSTREAM_REQUEST_TIMEOUT_MS` | Overrides the TOML request timeout |
| `MAX_UPSTREAM_RESPONSE_BYTES` | Overrides the TOML response size cap |

## TODOs (Next Steps)

**Phase 2 — make the control-plane management API usable** (in progress next)
1. **Contract-first resources**: ✅ v1 API done (`/v1/config`, `/v1/daemons`, paginated
   `/v1/usage/events`). Still open: per-model CRUD (`/v1/models/{model_name}`), daemon commands,
   config history/rollback, and enforcing the declared bearer auth (ADR 0001 step 2).
2. **Secrets**: `parameters.api_key` is plaintext today, in the contract, in memory, and in the
   WebSocket broadcast. Make it `writeOnly` and never echo it; prefer `api_key_env`.
3. **Virtual keys**: control-plane-issued client keys, replacing the daemon-local
   `DAEMON_API_KEYS` scaffolding from Phase 1.
4. **Persistence** (PostgreSQL, ADR 0001 steps 3–4) behind the `MemoryStore` API so config,
   presence, and usage survive restarts and multiple instances can run.
5. **Desired vs applied**: daemon sends a config ack so the dashboard shows in-sync/pending.
6. **Dashboard**: model list + form instead of the raw-JSON editor, key management, telemetry filters.

**Then**
7. **Improve reconnection logic**: Replace the fixed retry delay with exponential backoff and jitter.
8. **Validate `api_base` against its declared `format: uri`** in `validate_config` (today the
   contract documents it and the generators type it, but the handler only checks for empty).
9. **Authenticate the control channel**: it is unauthenticated, so secrets crossing it are only
   as safe as the network. Daemon enrolment tokens (+ TLS) should land before the 3 clusters.
10. **Dockerize** both control-plane and the daemon for the 3 clusters.
11. **Test with real upstreams** (DeepSeek, local vLLM, etc.) in the daemon.
12. **Document deployment** for ssh 198 / CarHer Pro environment.
