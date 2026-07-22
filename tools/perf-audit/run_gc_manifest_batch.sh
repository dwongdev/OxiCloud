#!/usr/bin/env bash
# Run the dedup-GC phase-1 benchmark against a fresh database in the existing
# local PostgreSQL container. The trap drops the database even on interruption.
set -euo pipefail

container="${OXICLOUD_POSTGRES_CONTAINER:-oxicloud-postgres-1}"
db="oxicloud_perf_gc_${$}_${RANDOM}"
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
pg_host="${OXICLOUD_POSTGRES_HOST:-127.0.0.1}"

cleanup() {
  docker exec "$container" dropdb --if-exists --force -U postgres "$db" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

docker exec "$container" createdb -U postgres "$db"

# Pin IPv4 and disable TLS explicitly. The local container exposes plain TCP;
# avoiding localhost/SSL negotiation keeps the harness independent of host
# resolver and optional SQLx TLS features.
export DATABASE_URL="postgres://postgres:postgres@${pg_host}:5432/$db?sslmode=disable"
export GC_MANIFEST_COUNTS="${GC_MANIFEST_COUNTS:-10000,50000}"
export GC_CHUNKS_PER_MANIFEST="${GC_CHUNKS_PER_MANIFEST:-16}"
export GC_SHARED_PERCENT="${GC_SHARED_PERCENT:-50}"
export GC_SHARED_POOL="${GC_SHARED_POOL:-512}"
export GC_WARMUPS="${GC_WARMUPS:-1}"
export GC_SAMPLES="${GC_SAMPLES:-5}"

cargo run \
  --release \
  --manifest-path "$repo_root/tools/perf-audit/Cargo.toml" \
  --bin gc_manifest_batch
