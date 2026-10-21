#!/usr/bin/env bash
# Regenerate every language binding from contract/openapi.yaml.
#
#   scripts/generate-types.sh          regenerate Rust DTOs and TypeScript types
#   scripts/generate-types.sh --check  fail if the committed output is stale
#
# Requires the Rust generator CLI:
#   cargo install --locked openapi-to-rust
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "${1:-}" == "--check" ]]; then
  echo "== Rust DTOs (protocol/src/generated) =="
  (cd "$root" && openapi-to-rust generate --config openapi-to-rust.toml --check)

  echo "== TypeScript types (web/src/api/schema.d.ts) =="
  (cd "$root/web" && npm run --silent generate:api)
  if git -C "$root" rev-parse --git-dir >/dev/null 2>&1; then
    if ! git -C "$root" diff --exit-code -- web/src/api/schema.d.ts >/dev/null; then
      echo "error: web/src/api/schema.d.ts is stale; run scripts/generate-types.sh"
      exit 1
    fi
    echo "up to date"
  else
    echo "skipped the TypeScript diff: $root is not a git work tree"
  fi
  exit 0
fi

echo "== Rust DTOs -> protocol/src/generated =="
(cd "$root" && openapi-to-rust generate --config openapi-to-rust.toml)

echo "== TypeScript types -> web/src/api/schema.d.ts =="
(cd "$root/web" && npm run --silent generate:api)
