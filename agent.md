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
        |                                       |  npm run build
        |                                       v
        |                               web/dist/  --rust-embed--> control-plane binary
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
        /control (WS), /api/* (JSON), / (dashboard)            OpenAI-compatible /v1/*
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
   (`control-plane/src/main.rs`), which parses the contract and asserts that every
   declared path/method exists on the axum router, that `/api/status` exposes every
   contract-required field, that nullable fields serialize as explicit `null`s, and that
   undeclared `/api/*` paths return JSON 404.
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
  - Serves the JSON API (`/api/status`, `/api/config`) using the generated DTOs
  - Serves the embedded dashboard; unknown `/api/*` paths return JSON 404, all other
    paths fall back to the SPA shell
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
- Implemented **Dashboard** (`web/`): React 19 + TypeScript + Tailwind 4 + Vite, embedded with `rust-embed`
  - Connected daemons table, recent telemetry, model configuration editor that pushes to all daemons
  - Vitest + jsdom component tests (`npm test`)
- Added `scripts/generate-types.sh` to regenerate both bindings, with a `--check` mode

## Current Status
- Control Plane runs on port 3000 and serves `/control` (WebSocket), `/api/*` (JSON), and `/` (embedded dashboard)
- Daemon serves an OpenAI-compatible API on port 4000 and reacts to pushed configuration
- **Phase 1 complete**: the daemon inference endpoint reports real usage. Verified end-to-end
  against a mock upstream: a unary call reported 17/5 tokens, a streaming call reported 21/13
  tokens with the usage frame deliberately split across two socket writes, an upstream that
  omits `usage` was recorded as `usage_reported: false` with zero tokens rather than a silent
  zero, unauthenticated calls were rejected with 401 before reaching the upstream, and
  `/healthz` reported zero dropped telemetry
- Earlier end-to-end checks still hold: config pushed through `/api/config` is broadcast, a live
  daemon immediately serves the new model list, `upstream_model` rewrites the model id sent
  upstream, and `api_key_env` injects the upstream bearer token
- `cargo test --workspace` passes: 5 contract tests and 13 daemon tests; `openapi-to-rust --check`,
  `npm test` and `npm run typecheck` pass
- POC demonstrates centralized config distribution without sending inference traffic through the control plane

## Build & Run

```sh
# One-time: the Rust contract generator
cargo install --locked openapi-to-rust

# Regenerate bindings after editing contract/openapi.yaml
scripts/generate-types.sh

# Dashboard (only needed when changing the frontend or the contract)
cd web && npm install && npm run build     # writes web/dist/, embedded by rust-embed

# Backend
cargo run -p control-plane                 # http://localhost:3000/
cargo build --release -p daemon            # target/release/daemon
target/release/daemon --cp ws://127.0.0.1:4000/control --config daemon/config.example.toml

# Checks
cargo test --workspace                     # contract drift guard
cargo fmt --all -- --check
scripts/generate-types.sh --check          # committed bindings are current
cd web && npm test && npm run typecheck
```

`web/dist/` is git-ignored apart from a marker file, so `cargo build` works in a
fresh checkout before the frontend is ever built: the API is fully functional and
`/` serves a "dashboard not built" page explaining the command to run.

### Daemon configuration

The daemon is a standalone binary. `--cp` sets the reachable control-plane
WebSocket endpoint; `--config` points to a daemon-local TOML file (see
`daemon/config.example.toml`). Its API listener defaults to `0.0.0.0:0`, so the
OS selects an available port and the daemon logs the chosen address.
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
1. **Contract-first resource CRUD**, replacing the single blob push: `GET/POST /api/models`,
   `GET/PUT/DELETE /api/models/{model_name}`, `GET /api/daemons`, `GET /api/daemons/{connection_id}`,
   `POST /api/daemons/{connection_id}/command`, and paginated `GET /api/telemetry`.
   Keep `POST /api/config` as a deprecated bulk operation.
2. **Secrets**: `parameters.api_key` is plaintext today, in the contract, in memory, and in the
   WebSocket broadcast. Make it `writeOnly` and never echo it; prefer `api_key_env`.
3. **Virtual keys**: control-plane-issued client keys, replacing the daemon-local
   `DAEMON_API_KEYS` scaffolding from Phase 1.
4. **Persistence** (SQLite) so models and keys survive a control-plane restart.
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
