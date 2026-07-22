#!/usr/bin/env bash
# Fresh-process max-RSS gate for owned String vs borrowed &str SQLx array binds.
# Each process runs one validated phase-1 sample; order alternates per repetition.
set -euo pipefail

container="${OXICLOUD_POSTGRES_CONTAINER:-oxicloud-postgres-1}"
pg_host="${OXICLOUD_POSTGRES_HOST:-127.0.0.1}"
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
binary="$repo_root/tools/perf-audit/target/release/gc_manifest_batch"
database="oxicloud_perf_gc_bind_${$}_${RANDOM}"
runs="${GC_RSS_RUNS:-5}"

cleanup() {
  docker exec "$container" dropdb --if-exists --force -U postgres "$database" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

cargo build --release --manifest-path "$repo_root/tools/perf-audit/Cargo.toml" --bin gc_manifest_batch
docker exec "$container" createdb -U postgres "$database"
database_url="postgres://postgres:postgres@${pg_host}:5432/${database}?sslmode=disable"

for scenario in 2:498 500:10 1000:10; do
  for ((run = 1; run <= runs; run++)); do
    modes=(current owned borrowed sorted)
    rotation=$(((run - 1) % 4))
    modes=("${modes[@]:rotation}" "${modes[@]:0:rotation}")
    for mode in "${modes[@]}"; do
      exclude_baselines=1
      include_cte=0
      owned_threshold=""
      borrowed_threshold=""
      sorted_threshold=""
      case "$mode" in
        current) exclude_baselines=0 ;;
        owned) owned_threshold=2 ;;
        borrowed) borrowed_threshold=2 ;;
        sorted) sorted_threshold=2 ;;
      esac
      echo "scenario=$scenario run=$run mode=$mode"
      /usr/bin/time -l env \
        DATABASE_URL="$database_url" \
        GC_SCENARIOS="$scenario" \
        GC_EXCLUDE_BASELINES="$exclude_baselines" \
        GC_INCLUDE_CTE="$include_cte" \
        GC_HYBRID_THRESHOLDS="$owned_threshold" \
        GC_BORROWED_THRESHOLDS="$borrowed_threshold" \
        GC_SORTED_THRESHOLDS="$sorted_threshold" \
        GC_WARMUPS=0 \
        GC_SAMPLES=1 \
        "$binary" 2>&1 \
        | sed -n -e '/summary:/p' -e '/  median=/p' -e '/maximum resident set size/p'
    done
  done
done
