#!/usr/bin/env bash
# PostgreSQL-backed integrity materialization/streaming A/B. The database is
# disposable and is dropped on success, failure, or interruption.
set -euo pipefail

container="${OXICLOUD_POSTGRES_CONTAINER:-oxicloud-postgres-1}"
database="oxicloud_perf_integrity_${$}_${RANDOM}"
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
host="${OXICLOUD_POSTGRES_HOST:-$(
  docker inspect -f '{{range.NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$container"
)}"

cleanup() {
  docker exec "$container" dropdb --if-exists --force -U postgres "$database" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

docker exec "$container" createdb -U postgres "$database"
export DATABASE_URL="postgres://postgres:postgres@${host}:5432/${database}?sslmode=disable"

cargo build --release --manifest-path "$repo_root/tools/perf-audit/Cargo.toml" \
  --bin verify_integrity_streaming
binary="$repo_root/tools/perf-audit/target/release/verify_integrity_streaming"
samples="${INTEGRITY_STREAM_SAMPLES:-7}"

"$binary" seed
for scenario in empty one four semantics shared unique large_manifest large; do
  "$binary" compare "$scenario"
done

# Warm PostgreSQL/OS caches once; warm-up output and RSS are not measurements.
for mode in historical materialized streaming; do
  "$binary" run "$mode" large full >/dev/null
done

for scenario in empty one four shared unique large_manifest large; do
  for ((sample = 0; sample < samples; sample++)); do
    case $((sample % 3)) in
      0) modes=(historical materialized streaming) ;;
      1) modes=(materialized streaming historical) ;;
      2) modes=(streaming historical materialized) ;;
    esac
    for mode in "${modes[@]}"; do
      { /usr/bin/time -l "$binary" run "$mode" "$scenario" full; } 2>&1 \
        | rg 'mode=|maximum resident set size'
    done
  done
done
