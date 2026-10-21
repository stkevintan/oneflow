# L2 — Detailed design

Each page describes one component from [L1](../L1-system-design.md) at the
level of modules, data structures, and algorithms.

| Page | Covers | Main source |
| --- | --- | --- |
| [protocol.md](./protocol.md) | HTTP contract, code generation, WebSocket messages | `contract/`, `protocol/` |
| [control-plane.md](./control-plane.md) | Routes, shared state, broadcast, WebSocket lifecycle | `control-plane/src/` |
| [daemon.md](./daemon.md) | CLI, control channel, inference pipeline, usage accounting | `daemon/src/` |
| [web.md](./web.md) | Dashboard components, polling, build and embedding | `web/` |
| [configuration.md](./configuration.md) | Shared `config.toml`, daemon TOML, environment variables | `config-provider/`, `config.toml`, `daemon/config.example.toml` |
