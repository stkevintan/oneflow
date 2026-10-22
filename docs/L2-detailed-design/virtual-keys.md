# Virtual keys (proposed; not implemented)

This document specifies the next access-control feature, not current behavior.
Today `DAEMON_API_KEYS` is a local shared-key list, management authentication is
not enforced, and the control channel is not authenticated. None of those
mechanisms is sufficient to deploy the design below safely.

## Scope and contracts

Initial scope: creation, metadata listing/lookup, permanent revocation, optional
key expiry, and explicit model allowlists. Keys are immutable except for
revocation; rotate by creating a replacement, updating the app, then revoking
the old key. Budgets, rate limits, teams, and per-user RBAC are deferred.

- [Proposed management OpenAPI](../../contract/virtual-keys.openapi.yaml)
- [Proposed control-message JSON Schema](../../contract/virtual-key-control.schema.json)

These contracts are intentionally separate from the implemented
[management contract](../../contract/openapi.yaml). Existing generators and
router drift tests continue to target only the implemented contract.
Do not add runtime routes, generated bindings, or Rust protocol variants until
implementation begins.

## Credentials and request flow

Three credentials have separate purposes:

| Credential | Used by | Accepted by |
| --- | --- | --- |
| Administrator credential | Management client | Control-plane management API only |
| Virtual key | External inference app | Daemon `/v1/*` only |
| Provider credential | Daemon | Upstream provider only |

An app configures base URL `http://127.0.0.1:9000/v1`, its virtual key as the
OpenAI API key, and an allowed public `model_name` as the model.

1. Authenticate the bearer key against the current leased policy snapshot.
2. Authorize the requested public model name, before `upstream_model` rewriting.
3. Resolve the model and upstream credentials.
4. Proxy the request and attribute usage to the non-secret key ID.

The virtual key MUST NOT be forwarded upstream, included in payloads, logged,
or persisted in telemetry. The existing fallback that forwards the client's
Authorization header must be disabled in virtual-key mode. Missing configured
provider credentials must not trigger that fallback: a missing referenced
`api_key_env` is an explicit upstream-auth configuration error (503).
Providers that intentionally need no credential may receive no Authorization
header. Literal provider keys remain supported but environment references are
preferred.

## Key lifecycle and storage

The control plane generates a random 128-bit ID (32 lowercase hex characters)
and an independent cryptographically random 256-bit secret. The token format
is `ofk_<id>_<secret hex>`. Store only SHA-256 of the complete UTF-8 token,
alongside metadata; never accept the hash itself as a credential. High-entropy
server-generated secrets make a fast hash suitable here; this is not password
storage. Daemons extract the ID, look up its policy, and compare the token hash
in constant time.

Creation returns the plaintext token once with `Cache-Control: no-store`.
Read/list/revoke responses expose neither token nor hash. Redact request
Authorization headers and creation response bodies in application logs,
reverse proxies, tracing, and audit records. All management responses involving
keys should be non-cacheable.

The create response is not recoverable. If it is lost, list metadata, identify
the orphaned key by ID, revoke it, and create a replacement. Display names are
not unique and must never be used as identifiers.

Expiry is checked at request admission: `now >= expires_at` rejects a key.
Null expiry means no key-specific expiry, not exemption from the policy lease.
Revocation is permanent, timestamped once, and idempotent. Revoked/expired
metadata remains readable; snapshots omit revoked keys. Expired keys may remain
in snapshots but cannot authorize requests. Any cleanup that removes them must
increment the revision, so identical revisions always have identical policies.
Admitted requests,
including streams, are not cancelled by later expiry or revocation.

Use durable PostgreSQL storage before exposing creation. Store key rows and a
singleton monotonic policy revision in transactions; creation/revocation
increments the revision in the same transaction. Do not recreate usable keys
from defaults after restart or claim success when storage is unavailable.
Notifications are hints: periodic refresh reads a transactionally consistent
revision and non-revoked policy set from the database, not a stale per-process cache.
Serialize mutations against model-config updates when validating allowlists.

Listing uses keyset pagination on `(created_at, id)` descending, not offsets.
Concurrent insertions appear on a new traversal; returned metadata reflects
current revocation state. Invalid or inconsistent cursors return 400.

## Model permissions and inference errors

`allowed_models` is a non-empty list of exact public model names. Reject blank
names, duplicates, wildcard `*`, and models absent at creation. There is no
implicit all-models permission. Removing a model never falls back to another
model. Reusing the same public name grants access to its new configuration:
the permission attaches to the alias, not a specific upstream provider.

| Request/outcome | HTTP | OpenAI error code |
| --- | --- | --- |
| Missing, malformed, unknown, expired, or revoked key | 401 | `invalid_api_key` |
| Valid key, model outside its allowlist | 403 | `model_not_allowed` |
| No valid policy lease (including startup) | 503 | `auth_policy_unavailable` |
| Valid key and permission, but model no longer configured | 404 | `model_not_found` |
| Missing/invalid model field in chat request after authentication | 400 | `invalid_request` |
| Referenced provider credential unavailable | 503 | `upstream_auth_unavailable` |

Error body shape:

```json
{
  "error": {
    "message": "The virtual key cannot access this model",
    "type": "permission_error",
    "param": "model",
    "code": "model_not_allowed"
  }
}
```

Use `authentication_error` for 401, `permission_error` for 403,
`invalid_request_error` for 400/404, and `server_error` for 503.
`param` is `"model"` for model-related failures and null otherwise. 401 also
includes `WWW-Authenticate: Bearer`. Errors must not reveal whether a supplied
key was revoked, expired, or unknown.

Validate bearer syntax first, then policy availability, then key validity and
model permission. With an unavailable policy, syntactically valid tokens get
503, not a misleading invalid-key response.

`GET /v1/models` returns only configured models in the caller's allowlist;
`GET /v1/models/{model_name}` enforces the same permission as chat completions.
`/healthz` remains unauthenticated and reveals no secrets. Admission failures
make no upstream call. Usage events gain a nullable `virtual_key_id` (never a
secret/hash); legacy events have null. Rejected requests are recorded in
sanitized auth logs rather than invented upstream usage events.

## Distribution and bounded offline lease

Use full snapshots initially, not deltas. The new control capability is
`virtual-keys-v1`. The control plane sends a snapshot after authenticated
registration, on mutations, and every 60 seconds to refresh its lease.
All instances read the durable policy state before issuing a fresh lease.

A snapshot carries a global revision, `issued_at`, absolute `valid_until`,
and the complete non-revoked policy set. Empty snapshots revoke all access.
Atomically replace the cache only after validating the entire message;
duplicate key IDs, malformed entries, or invalid leases reject the whole
snapshot with an explicit error log. Preserve the prior cache only until its
original deadline. Send `VirtualKeyPolicyAck` only after successful apply.
Acknowledgements echo revision and deadline and are associated with the
authenticated connection; expose last acknowledgement and lease deadline in
future daemon status so operators can inspect propagation.

- Lease duration: at most **300 seconds** from issuance, not receipt.
- Accept only `issued_at < valid_until <= issued_at + 300`, an unexpired
  deadline, and an issuance time no more than 30 seconds ahead of local time.
- Lower revisions are ignored and logged. Equal revisions may renew the lease
  only with identical policy contents and a newer issuance time/deadline.
  Repeated/replayed frames never extend a deadline.
- Enforce the absolute deadline and a monotonic timer derived on receipt;
  use whichever expires first so wall-clock rollback cannot extend a lease.
- Startup/restart has no authorized cache: fail closed until initial sync.
- On disconnect, use the cached policy until its original lease expires,
  then return 503 on protected inference requests, even if the socket is open.
- Reconnect alone does not restore access; a valid fresh snapshot must apply.
- Keep clocks synchronized within 30 seconds. Revocation lag is bounded by
  the remaining 300-second lease plus at most 30 seconds of clock skew;
  admitted streams may outlive that window.

On a connected daemon revocation is normally effective after the next snapshot
is applied, but HTTP revocation success guarantees durable storage, not global
application. A connected-but-stale daemon is also fenced by its lease.

## Deployment prerequisites and compatibility

Require administrator authentication for the management API and authenticated,
encrypted daemon control connections before distributing key policies. Bind
daemon identities to credentials rather than trusting a claimed `daemon_id`.
Hashes are not bearer secrets, but the control channel can change all access
policies and must be trusted. Non-loopback inference clients need HTTPS.

Add explicit daemon `auth_mode = "virtual_keys"` as an opt-in rollout setting.
Initially preserve legacy `DAEMON_API_KEYS` behavior in legacy mode. In
virtual-key mode, never fall back to those static keys or anonymous access;
reject startup if static keys are configured, to avoid ambiguous authorization.
Model configuration updates and policy snapshots are separate: authorization
always intersects the granted aliases with the currently configured models.

Old daemons cannot enforce virtual policies: block enrolling them into a
virtual-key deployment. Do not send unsupported variants and assume they are
enforced. Roll out capabilities, then authenticated snapshot support, then the
explicit auth mode; migrate apps before removing static keys.

## Implementation acceptance criteria

1. Management auth rejects virtual keys; storage survives restarts; creation
   reveals a token once; list/read/revoke never reveal token/hash.
2. Validation covers future/null expiry, unknown/duplicate/wildcard models,
   pagination, lost-create-response handling, and idempotent revocation.
3. Inference covers the error table, model-list filtering, alias rewriting,
   unchanged inference forwarding/streaming, and no virtual-key leakage upstream.
4. Fake-clock tests cover exactly-at-expiry, the 300-second maximum,
   30-second skew, disconnect, stalled refresh on an open socket, replay,
   duplicate IDs, rollback, and reconnect with no snapshot.
5. Multi-instance tests cover durable revision ordering, mutation/refresh
   races, acknowledgements, and an empty snapshot that removes every key.
6. Legacy mode remains unchanged; virtual-key mode cannot silently downgrade.
7. At implementation time, merge the management proposal into the live
   contract, regenerate bindings, implement the protocol schema in Rust,
   extend daemon status/usage contracts, and run router drift and integration
   tests. Remove proposal-only files once the live contracts replace them.
