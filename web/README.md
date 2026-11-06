# Control Plane Dashboard

React + TypeScript + Tailwind client for the `control-plane` service. The control
plane is a pure API service (`/v1/*` and `/control`) and serves no UI, so the
dashboard is deployed on its own, like any other client (CLI, TUI, desktop); see
[ADR 0001](../docs/adr/0001-api-only-stateless-control-plane.md).

## Daemon provisioning

Apply the required administrator token and use
**Daemon credentials** to create an ID, then click **Download config.toml**.
It includes the configured control-plane URL, daemon ID, generated token, and
loopback listener. Secure the downloaded file and run `oneflowd -c config.toml`.
There is no authentication-mode setting. The one-time result stays only in page
memory; dismiss after downloading. The list includes offline and revoked daemons. Rotation/revocation
require confirmation; rotation also reactivates a revoked credential. Restart
the daemon after downloading/deploying its replacement configuration.

Credential metadata is separate from the live connection table. Authentication
and storage failures are surfaced explicitly; provisioning is never anonymous.
See [deployment](../docs/virtual-keys.md#3-start-a-virtual-key-daemon).

## Daemon details and consumer keys

Click a daemon ID in **Connected daemons** or **Daemon credentials** to open
its detail page. The URL uses `#daemon=<id>`, supports browser back/forward,
and can be bookmarked (the administrator token still needs to be applied after
reload). Offline and revoked daemons have detail pages too.

The **Consumer virtual keys** section creates credentials for applications
calling that daemon's OpenAI-compatible endpoint. Enter a consumer name, choose
allowed model aliases, optionally set an expiry such as `30d`, and click
**Create virtual key**. All currently configured aliases are initially selected;
at least one is required. Copy the one-time `sk-...` key before dismissing it
or leaving the page. Nothing is saved in browser storage.

Keys created here are bound to the selected daemon and are rejected by other
daemons, even when they expose the same models. Listing is paginated and shows
metadata only; revocation requires confirmation. Previously issued unbound keys
remain cross-daemon and are not included in this daemon-filtered list.
Use the consumer key as the app's API key, not as a provider credential or
daemon control token. Use the daemon's reachable `/v1` endpoint as the app's
base URL.

Upgrade both the control plane and daemons before issuing daemon-bound keys.
The control plane applies the database migration on startup. Older daemons
reject policies containing the new scope field and fail closed after their
existing lease expires.

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
  `control-plane/src/api.rs`, which parses the contract and asserts that every
  declared path/method actually exists on the axum router, that every GET
  response carries every contract-required field, and that unknown paths return
  JSON 404.

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
| `src/api/client.ts`      | Typed `/v1` client and generated type aliases         |
| `src/api/useDashboard.ts`| Polling hook for status, daemons, usage, and config  |
| `src/env.d.ts`           | Type for `VITE_CONTROL_PLANE_URL`                    |
| `src/components/`        | `Topbar`, `Card`, `DaemonTable`, `TelemetryTable`, `ModelList`, `ModelForm`, `ConfigEditor` |
| `src/App.test.tsx`       | Renders the dashboard against a stubbed API          |
| `dist/`                  | Build output (git-ignored), deployed separately      |

## Commands

```sh
# From the repository root, first: cp config.example.toml config.toml
npm install
npm run generate:api   # regenerate types from contract/openapi.yaml
npm run dev            # address and API proxy are read from ../config.toml
npm run build          # generate:api + typecheck + vite build -> dist/
npm run preview        # serve dist/ on the same address, with the same /v1 proxy
npm test               # vitest + jsdom component tests
npm run typecheck
```

The user-created, Git-ignored `../config.toml` (copied from
[`config.example.toml`](../config.example.toml)) configures the control-plane
address and the address of the dev and preview servers. Both servers proxy
`/v1` and `/key` to the control plane, so
the browser sees a single origin and no CORS setup is needed. Set `LLMCP_CONFIG`
to use a different TOML file. The daemon is a standalone executable with its own
TOML file and explicit `--cp` WebSocket endpoint; see
[`../daemon/README.md`](../daemon/README.md).

Run `cargo run -p control-plane` from the repository root alongside `npm run dev`
(or `npm run preview`) so the proxy has something to talk to.

## Hosting the bundle elsewhere

`dist/` is plain static files and can be served by any static host. When it is
not served through the proxy, build it with the control-plane URL and allow that
origin on the control plane:

```sh
VITE_CONTROL_PLANE_URL=https://cp.example npm run build
```

```toml
# ../config.toml on the control-plane host
[control_plane]
cors_allowed_origins = ["https://dashboard.example"]
```

Enter the management credential in the dashboard's
**Administrator token** form. It is sent as a bearer token, kept only in
memory, and cleared on page reload. Never bake it into the Vite bundle.
See
[virtual-key deployment](../docs/virtual-keys.md).
