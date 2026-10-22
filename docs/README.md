# Architecture documentation

The design is documented at three levels of detail. Each level zooms into the
one above it; read them in order.

| Level | Document | Audience | Answers |
| --- | --- | --- | --- |
| L0 | [System context](./L0-architecture.md) | Everyone | What the system is, who uses it, what it talks to |
| L1 | [Containers and flows](./L1-system-design.md) | Engineers, operators | Which deployable units exist, how they communicate, how they are deployed |
| L2 | [Detailed design](./L2-detailed-design/README.md) | Contributors | How each unit is built internally |

L2 is split by component:

- [Protocol and contract](./L2-detailed-design/protocol.md)
- [Control plane](./L2-detailed-design/control-plane.md)
- [Daemon](./L2-detailed-design/daemon.md)
- [Web dashboard](./L2-detailed-design/web.md)
- [Configuration](./L2-detailed-design/configuration.md)
- [Virtual-key deployment and usage](./virtual-keys.md)
- [Virtual-key design](./L2-detailed-design/virtual-keys.md)

These documents describe the code as it is, and mark where an accepted
decision changes it. Planned work is listed under "Known gaps" in
[L1](./L1-system-design.md#known-gaps) and in [`../agent.md`](../agent.md).

## Architecture decision records

| ADR | Title | Status |
| --- | --- | --- |
| [0001](./adr/0001-api-only-stateless-control-plane.md) | API-only, stateless control plane (PostgreSQL, `LISTEN`/`NOTIFY`) | Accepted; step 1 (API-only `/v1`) implemented |
