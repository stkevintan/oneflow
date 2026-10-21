# Control Plane Dashboard

React + TypeScript + Tailwind frontend for the `control-plane` service. The built
bundle is **embedded into the control-plane binary at compile time** with
[`rust-embed`](https://crates.io/crates/rust-embed), so there is no separate
deployment step: `cargo run -p control-plane` serves the dashboard at `/`.

## Contract-first workflow

[`contract/openapi.yaml`](../contract/openapi.yaml) is the source of truth for the
JSON API.

```
contract/openapi.yaml          <- edited first, reviewed like code
        |
        |  npm run generate:api      (openapi-typescript)
        v
web/src/api/schema.d.ts        <- generated, never hand-edited
        |
        v
web/src/api/client.ts          <- hand-written typed fetch wrapper
```

On the Rust side both layers consume the same contract:

* the **DTOs** are generated into `protocol/src/generated/` by
  [`openapi-to-rust`](https://openapi-to-rust.dev) (config: `openapi-to-rust.toml`),
* the **handlers** stay hand-written, and `cargo test` runs `contract_tests` in
  `control-plane/src/main.rs`, which parses the contract and asserts that every
  declared path/method actually exists on the axum router, that `/api/status`
  exposes every contract-required field, and that undeclared `/api/*` paths
  return JSON 404.

A route renamed in Rust without updating the contract (or vice versa) fails the
build. Regenerate all bindings with [`scripts/generate-types.sh`](../scripts/generate-types.sh);
`--check` fails when committed output is stale.

## Layout

| Path                     | Purpose                                              |
| ------------------------ | ---------------------------------------------------- |
| `index.html`             | Vite entry document                                  |
| `src/main.tsx`           | React bootstrap                                       |
| `src/App.tsx`            | Dashboard composition                                |
| `src/api/schema.d.ts`    | Generated from the contract                          |
| `src/api/client.ts`      | Typed API client and generated type aliases          |
| `src/api/useStatus.ts`   | Polling hook for `GET /api/status`                   |
| `src/components/`        | `Topbar`, `Card`, `DaemonTable`, `TelemetryTable`, `ConfigEditor` |
| `src/App.test.tsx`       | Renders the dashboard against a stubbed API          |
| `scripts/ensure-dist.mjs`| Restores the `dist/` marker after `vite build` clears it |
| `dist/`                  | Build output, embedded by rust-embed                 |

## Commands

```sh
npm install
npm run generate:api   # regenerate types from contract/openapi.yaml
npm run dev            # address and API proxy are read from ../config.toml
npm run build          # generate:api + typecheck + vite build -> dist/
npm test               # vitest + jsdom component tests
npm run typecheck
```

The shared `../config.toml` configures the control-plane bind address and the
web development server address. The production dashboard is embedded and served
by the control plane. The daemon is a standalone executable with its own TOML
file and explicit `--cp` WebSocket endpoint; see
[`../daemon/README.md`](../daemon/README.md).

Run `cargo run -p control-plane` from the repository root alongside `npm run
dev` so the `/api` proxy has something to talk to. Set `LLMCP_CONFIG` to use a
different TOML file. For the embedded build run `npm run build` first, then
`cargo run -p control-plane` and open the configured control-plane address.

## Fresh checkout

`web/dist/` contains a committed placeholder page and is otherwise git-ignored.
That means `cargo build` works before the frontend has ever been built: the
control-plane compiles, the JSON API is fully functional, and `/` serves the
"frontend not built" placeholder instead of the dashboard.
