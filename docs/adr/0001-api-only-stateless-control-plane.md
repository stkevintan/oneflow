# ADR 0001 — API-only, stateless control plane

- **Status:** Accepted. Step 1 of the [migration plan](#migration-plan) is
  implemented; steps 2–5 are not.
- **Date:** 2026-10-21
- **Supersedes:** "Dashboard embedded in the control-plane binary" in the
  original L0 key decisions

## Context

The proof of concept ships one `control-plane` binary that:

1. embeds the React dashboard (`web/dist` via `rust-embed`) and serves it with
   an SPA fallback;
2. keeps the desired model config, the connected-daemon registry, and recent
   telemetry in process memory (`Arc<RwLock<SharedState>>`);
3. fans config changes out to daemons with an in-process `tokio::broadcast`.

This is convenient for a demo but has three problems:

- **The UI is a privileged client.** We expect several clients (web, CLI, TUI,
  desktop). Embedding one of them couples the control-plane build and release
  cadence to it.
- **A restart loses everything.** Config reverts to a built-in default, and
  telemetry history is lost.
- **It cannot scale out or fail over.** State and fan-out are local to one
  process, so a second instance would show a different set of daemons and miss
  config pushes made on the first one.

## Decision

### 1. The control plane is a pure API service

- It serves only `/control` (daemon WebSocket) and a versioned JSON API under
  `/v1/...`. It serves no HTML and no static assets.
- `contract/openapi.yaml` stays the single source of truth. Every client
  generates or reuses types from it.
- All clients are equal and deployed independently:

  | Client | Form | Types from |
  | --- | --- | --- |
  | Web dashboard | Static SPA, run locally or on any static host | `openapi-typescript` |
  | CLI | Rust binary | `protocol` crate |
  | TUI | Rust binary (for example `ratatui`) | `protocol` crate |
  | Desktop | Wrapper around the web UI (for example Tauri) | `openapi-typescript` |

- Because browser clients are now cross-origin, the API needs a **CORS
  allow-list** (in `config.toml`) and **bearer-token authentication**.

### 2. The control-plane process is stateless; PostgreSQL holds the state

Every instance is interchangeable. Any instance can be stopped or added
without losing data, and any instance can answer any API request.

| State | Where it lives | Notes |
| --- | --- | --- |
| Desired model config | PostgreSQL `config_versions` (append-only) | The latest row is the desired config; history allows audit and rollback |
| Daemon registry / presence | PostgreSQL `daemon_sessions` | Upserted on registration and heartbeat; a row is live while `last_seen` is fresh |
| Per-request usage | PostgreSQL `usage_events` (batched inserts) | Optionally also exported via OTLP |
| Live WebSocket | Memory of the instance that accepted it | Unavoidable; lost on restart, recovered by daemon reconnect |

### 3. Cross-instance fan-out uses PostgreSQL `LISTEN`/`NOTIFY`

No extra infrastructure: the database that stores the config also announces
changes. The payload is only a pointer (`config_versions.id`); instances read
the row itself, because `NOTIFY` payloads are limited to 8000 bytes and
notifications are not durable.

## Design sketch

```mermaid
sequenceDiagram
    participant C as Client (web/CLI/TUI)
    participant A as CP instance A
    participant PG as PostgreSQL
    participant B as CP instance B
    participant D as Daemon (socket on B)
    C->>A: PUT /v1/config (bearer token)
    A->>PG: BEGIN; INSERT config_versions; NOTIFY config_changed, id; COMMIT
    A-->>C: 200
    PG--)A: notification(id)
    PG--)B: notification(id)
    B->>PG: SELECT config by id
    B->>D: ConfigUpdate
```

Rules that keep this correct:

- **Notify inside the transaction.** Postgres delivers `NOTIFY` only on
  commit, so no instance can see a notification for an uncommitted row.
- **Full snapshots, not diffs.** Every `ConfigUpdate` carries the whole config,
  so a missed notification is repaired by sending the latest row.
- **Resync on doubt.** When an instance (re)establishes its `LISTEN`
  connection, or its local broadcast reports `Lagged`, it re-reads the latest
  config and pushes it to its sockets.
- **Initial config from the database.** On daemon connect, the instance reads
  the latest `config_versions` row, not a cached copy.
- **Presence is a lease.** Each instance has an `instance_id`. Heartbeats
  update `last_seen`. A daemon is listed as connected only if
  `last_seen > now() - 3 × heartbeat interval` (15 s). A clean disconnect
  deletes the row. On startup an instance deletes rows still tagged with its
  own `instance_id`, and a periodic sweep removes expired rows left by crashed
  instances.
- **Load balancing.** A WebSocket-capable load balancer spreads daemons
  across instances. No sticky sessions are needed for `/v1/*`. On instance
  shutdown, daemons reconnect (5 s retry) and land on another instance.

### Schema sketch

```sql
CREATE TABLE config_versions (
    id          BIGSERIAL PRIMARY KEY,
    version     TEXT        NOT NULL UNIQUE,
    models      JSONB       NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by  TEXT
);

CREATE TABLE daemon_sessions (
    connection_id   UUID PRIMARY KEY,
    instance_id     UUID        NOT NULL,
    daemon_id       TEXT,
    cluster_name    TEXT,
    hostname        TEXT,
    version         TEXT,
    capabilities    TEXT[]      NOT NULL DEFAULT '{}',
    loaded_models   TEXT[]      NOT NULL DEFAULT '{}',
    active_requests INTEGER     NOT NULL DEFAULT 0,
    connected_at    TIMESTAMPTZ NOT NULL,
    last_seen       TIMESTAMPTZ NOT NULL
);
CREATE INDEX ON daemon_sessions (last_seen);

CREATE TABLE usage_events (
    request_id        TEXT PRIMARY KEY,
    daemon_id         TEXT        NOT NULL,
    model_name        TEXT        NOT NULL,
    prompt_tokens     INTEGER     NOT NULL,
    completion_tokens INTEGER     NOT NULL,
    success           BOOLEAN     NOT NULL,
    status_code       SMALLINT,
    latency_ms        BIGINT      NOT NULL,
    streamed          BOOLEAN     NOT NULL,
    usage_reported    BOOLEAN     NOT NULL,
    received_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX ON usage_events (received_at);
```

`request_id` as the primary key makes telemetry inserts idempotent if a daemon
ever resends. Per-daemon totals in `GET /v1/daemons` become aggregate queries
instead of in-memory counters.

## Consequences

**Positive**

- Clients evolve and ship independently; adding a CLI or TUI needs no
  control-plane change.
- Config and usage survive restarts; config history enables audit and rollback.
- Horizontal scaling and rolling upgrades with no extra infrastructure beyond
  PostgreSQL.
- Fixes the "lagging daemon misses updates" gap through resync-on-doubt.

**Negative**

- Two deployables instead of one for a demo; the web client needs its own
  hosting or local run.
- PostgreSQL becomes a hard dependency. Inference is unaffected by a database
  outage, because daemons keep their last config. Config pushes and the status
  API fail until the database returns.
- Heartbeats become database writes, roughly N/5 per second for N daemons.
  This is fine for thousands of daemons; beyond that, batch the upserts or move
  presence to a cache.
- Each instance holds one dedicated `LISTEN` connection outside the pool.
- CORS and authentication become mandatory work, not optional hardening.

## Alternatives considered

| Alternative | Why not now |
| --- | --- |
| Keep the embedded dashboard and add API clients alongside it | Leaves the web UI privileged and couples releases |
| Redis for presence and pub/sub | Second piece of infrastructure; config still needs a durable store |
| NATS / Kafka for fan-out | Same; justified only at much larger scale |
| etcd watch | Strong fit for config, but weak for usage data and less familiar operationally |
| Single instance with SQLite | Survives restarts but does not scale out or fail over |

## Migration plan

1. ✅ **API-only.** Remove `assets.rs`, `rust-embed`, and the SPA fallback. Move
   routes to `/v1`. Add the CORS allow-list. Run the web client standalone.
   *Done:* the `/v1` contract (health, status, config, daemons, usage events)
   is implemented, with bearer auth declared but not enforced and no config
   history endpoint yet. State sits behind the `MemoryStore` API in
   `control-plane/src/store.rs`, ready to be swapped for PostgreSQL.
2. **Auth.** Bearer tokens for `/v1/*`; separate daemon credentials for
   `/control`.
3. **Durable config.** `config_versions` table, `LISTEN`/`NOTIFY` fan-out,
   resync rules.
4. **Presence and usage.** `daemon_sessions` and `usage_events`; `/v1/status`,
   `/v1/daemons`, and `/v1/usage/events` read from PostgreSQL.
5. **Second instance.** Put a load balancer in front and verify failover.
