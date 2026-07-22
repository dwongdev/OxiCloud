#!/usr/bin/env bash
# Short decisive gate for the bounded SQLx producer/channel variant.
set -euo pipefail

container="${OXICLOUD_POSTGRES_CONTAINER:-oxicloud-postgres-1}"
database="oxicloud_perf_integrity_prefetch_${$}_${RANDOM}"
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
host="${OXICLOUD_POSTGRES_HOST:-$(
  docker inspect -f '{{range.NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$container"
)}"
samples="${INTEGRITY_PREFETCH_SAMPLES:-7}"

cleanup() {
  docker exec "$container" dropdb --if-exists --force -U postgres "$database" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

docker exec "$container" createdb -U postgres "$database"
export DATABASE_URL="postgres://postgres:postgres@${host}:5432/${database}?sslmode=disable"
cargo build --release --manifest-path "$repo_root/tools/perf-audit/Cargo.toml" \
  --bin verify_integrity_streaming
binary="$repo_root/tools/perf-audit/target/release/verify_integrity_streaming"

"$binary" seed
for scenario in empty one four semantics shared unique large_manifest large; do
  "$binary" compare "$scenario"
done

"$binary" run historical large full >/dev/null
"$binary" run materialized large full >/dev/null
"$binary" run prefetch large full >/dev/null
for ((sample = 0; sample < samples; sample++)); do
  case $((sample % 3)) in
    0) modes=(historical materialized prefetch) ;;
    1) modes=(materialized prefetch historical) ;;
    2) modes=(prefetch historical materialized) ;;
  esac
  for mode in "${modes[@]}"; do
    { /usr/bin/time -l "$binary" run "$mode" large full; } 2>&1 \
      | rg 'mode=|maximum resident set size'
  done
done
