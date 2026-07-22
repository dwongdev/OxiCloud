//! Reproducible audit harness for `DedupService::garbage_collect_with_grace`
//! phase 1. This is deliberately outside `benches/` and changes no production
//! code.
//!
//! It compares:
//!   1. the current production shape: one DELETE/RETURNING per 500 rows plus
//!      one serial UPDATE for every returned manifest; and
//!   2. a proposed single CTE per batch that aggregates decrements by distinct
//!      chunk hash before updating `storage.blobs`; and
//!   3. hybrid thresholds that retain the simple DELETE/RETURNING and aggregate
//!      only the UPDATE after an exact per-manifest distinct pass in Rust.
//!
//! The runner creates a throw-away database. Within it this program keeps an
//! immutable fixture in `perf_audit.*` and restores the production-shaped
//! `storage.*` tables before every timed sample. Fixture/reset/validation time
//! is excluded from the reported duration.

use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, PgPool, Row};
use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::time::{Duration, Instant};

const BATCH_SIZE: i64 = 500;

const CURRENT_DELETE: &str = r#"
DELETE FROM storage.chunk_manifests
 WHERE ctid = ANY(
     SELECT ctid FROM storage.chunk_manifests m
      WHERE m.ref_count <= 0
         OR NOT EXISTS (
             SELECT 1 FROM storage.files f
              WHERE f.blob_hash = m.file_hash
         )
      LIMIT $1
 )
 RETURNING file_hash, chunk_hashes, total_size
"#;

const CURRENT_UPDATE: &str = r#"
UPDATE storage.blobs
   SET ref_count   = GREATEST(ref_count - 1, 0),
       orphaned_at = CASE
           WHEN GREATEST(ref_count - 1, 0) = 0 THEN now()
           ELSE orphaned_at
       END
 WHERE hash = ANY($1)
"#;

const AGGREGATED_UPDATE: &str = r#"
UPDATE storage.blobs b
   SET ref_count   = GREATEST(b.ref_count - d.decrement_by, 0),
       orphaned_at = CASE
           WHEN GREATEST(b.ref_count - d.decrement_by, 0) = 0 THEN now()
           ELSE b.orphaned_at
       END
  FROM unnest($1::text[], $2::integer[]) AS d(hash, decrement_by)
 WHERE b.hash = d.hash
"#;

// `SELECT DISTINCT` inside the LATERAL subquery is semantically important:
// production holds one blob reference per distinct chunk hash in a manifest,
// even when the same chunk occurs multiple times in that file. The current
// `hash = ANY($1)` likewise updates such a row only once per manifest.
const BATCHED_CTE: &str = r#"
WITH deleted AS MATERIALIZED (
    DELETE FROM storage.chunk_manifests
     WHERE ctid = ANY(
         SELECT ctid FROM storage.chunk_manifests m
          WHERE m.ref_count <= 0
             OR NOT EXISTS (
                 SELECT 1 FROM storage.files f
                  WHERE f.blob_hash = m.file_hash
             )
          LIMIT $1
     )
     RETURNING file_hash, chunk_hashes, total_size
), decrements AS MATERIALIZED (
    SELECT distinct_chunks.chunk_hash,
           COUNT(*)::integer AS decrement_by
      FROM deleted d
      CROSS JOIN LATERAL (
          SELECT DISTINCT chunk_hash
            FROM unnest(d.chunk_hashes) AS chunks(chunk_hash)
      ) AS distinct_chunks
     GROUP BY distinct_chunks.chunk_hash
), updated AS (
    UPDATE storage.blobs b
       SET ref_count = GREATEST(b.ref_count - d.decrement_by, 0),
           orphaned_at = CASE
               WHEN GREATEST(b.ref_count - d.decrement_by, 0) = 0 THEN now()
               ELSE b.orphaned_at
           END
      FROM decrements d
     WHERE b.hash = d.chunk_hash
     RETURNING b.hash
)
SELECT deleted.file_hash,
       deleted.chunk_hashes,
       deleted.total_size,
       (SELECT COUNT(*)::bigint FROM updated) AS updated_blob_rows
  FROM deleted
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AggregateMode {
    OwnedHashMap,
    BorrowedHashMap,
    SortedBorrowed { occurrence_window: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Algorithm {
    Current,
    Batched,
    Hybrid {
        aggregate_threshold: usize,
        aggregate_mode: AggregateMode,
    },
}

impl Algorithm {
    fn label(self) -> &'static str {
        match self {
            Self::Current => "current_n_plus_1",
            Self::Batched => "batched_cte",
            Self::Hybrid {
                aggregate_threshold: 2,
                aggregate_mode: AggregateMode::OwnedHashMap,
            } => "hybrid_n2",
            Self::Hybrid {
                aggregate_threshold: 4,
                aggregate_mode: AggregateMode::OwnedHashMap,
            } => "hybrid_n4",
            Self::Hybrid {
                aggregate_threshold: 8,
                aggregate_mode: AggregateMode::OwnedHashMap,
            } => "hybrid_n8",
            Self::Hybrid {
                aggregate_threshold: 32,
                aggregate_mode: AggregateMode::OwnedHashMap,
            } => "hybrid_n32",
            Self::Hybrid {
                aggregate_threshold: 500,
                aggregate_mode: AggregateMode::OwnedHashMap,
            } => "hybrid_n500",
            Self::Hybrid {
                aggregate_threshold: 2,
                aggregate_mode: AggregateMode::BorrowedHashMap,
            } => "hybrid_borrowed_n2",
            Self::Hybrid {
                aggregate_threshold: 4,
                aggregate_mode: AggregateMode::BorrowedHashMap,
            } => "hybrid_borrowed_n4",
            Self::Hybrid {
                aggregate_threshold: 8,
                aggregate_mode: AggregateMode::BorrowedHashMap,
            } => "hybrid_borrowed_n8",
            Self::Hybrid {
                aggregate_threshold: 32,
                aggregate_mode: AggregateMode::BorrowedHashMap,
            } => "hybrid_borrowed_n32",
            Self::Hybrid {
                aggregate_threshold: 500,
                aggregate_mode: AggregateMode::BorrowedHashMap,
            } => "hybrid_borrowed_n500",
            Self::Hybrid {
                aggregate_threshold: 2,
                aggregate_mode:
                    AggregateMode::SortedBorrowed {
                        occurrence_window: usize::MAX,
                    },
            } => "hybrid_sorted_n2",
            Self::Hybrid {
                aggregate_threshold: 4,
                aggregate_mode:
                    AggregateMode::SortedBorrowed {
                        occurrence_window: usize::MAX,
                    },
            } => "hybrid_sorted_n4",
            Self::Hybrid {
                aggregate_threshold: 8,
                aggregate_mode:
                    AggregateMode::SortedBorrowed {
                        occurrence_window: usize::MAX,
                    },
            } => "hybrid_sorted_n8",
            Self::Hybrid {
                aggregate_threshold: 32,
                aggregate_mode:
                    AggregateMode::SortedBorrowed {
                        occurrence_window: usize::MAX,
                    },
            } => "hybrid_sorted_n32",
            Self::Hybrid {
                aggregate_threshold: 500,
                aggregate_mode:
                    AggregateMode::SortedBorrowed {
                        occurrence_window: usize::MAX,
                    },
            } => "hybrid_sorted_n500",
            Self::Hybrid {
                aggregate_threshold: 32,
                aggregate_mode:
                    AggregateMode::SortedBorrowed {
                        occurrence_window: 512,
                    },
            } => "hybrid_sorted_n32_w512",
            Self::Hybrid {
                aggregate_threshold: 32,
                aggregate_mode:
                    AggregateMode::SortedBorrowed {
                        occurrence_window: 1024,
                    },
            } => "hybrid_sorted_n32_w1024",
            Self::Hybrid {
                aggregate_threshold: 32,
                aggregate_mode:
                    AggregateMode::SortedBorrowed {
                        occurrence_window: 2048,
                    },
            } => "hybrid_sorted_n32_w2048",
            Self::Hybrid {
                aggregate_threshold: 32,
                aggregate_mode:
                    AggregateMode::SortedBorrowed {
                        occurrence_window: 4096,
                    },
            } => "hybrid_sorted_n32_w4096",
            Self::Hybrid { .. } => "hybrid_other",
        }
    }
}

#[derive(Debug)]
struct RunOutcome {
    elapsed: Duration,
    statements: u64,
    deleted_manifests: u64,
    logical_bytes: u64,
    updated_blob_rows: u64,
    checksum: u64,
}

#[derive(Debug)]
struct FixtureStats {
    orphan_manifests: i64,
    live_manifests: i64,
    blob_rows: i64,
    duplicate_manifests: i64,
    orphan_logical_bytes: i64,
    chunks_per_manifest: i64,
}

#[derive(Clone, Copy, Debug)]
struct Scenario {
    orphan_manifests: i32,
    live_manifests: i32,
}

fn parse_usize(name: &str, default: usize) -> Result<usize, Box<dyn Error>> {
    match env::var(name) {
        Ok(raw) => Ok(raw.parse::<usize>().map_err(|e| {
            std::io::Error::other(format!("{name} must be an integer, got {raw:?}: {e}"))
        })?),
        Err(_) => Ok(default),
    }
}

fn parse_counts() -> Result<Vec<i32>, Box<dyn Error>> {
    let raw = env::var("GC_MANIFEST_COUNTS").unwrap_or_else(|_| "10000,50000".to_owned());
    let counts: Result<Vec<_>, _> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::parse::<i32>)
        .collect();
    let counts = counts.map_err(|e| {
        std::io::Error::other(format!(
            "GC_MANIFEST_COUNTS must be comma-separated integers, got {raw:?}: {e}"
        ))
    })?;
    if counts.is_empty() || counts.iter().any(|&n| n <= 0) {
        return Err(std::io::Error::other("manifest counts must all be positive").into());
    }
    Ok(counts)
}

fn parse_scenarios() -> Result<Vec<Scenario>, Box<dyn Error>> {
    if let Ok(raw) = env::var("GC_SCENARIOS") {
        let mut scenarios = Vec::new();
        for value in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (orphans, live) = value.split_once(':').ok_or_else(|| {
                std::io::Error::other(format!(
                    "GC_SCENARIOS entries must be orphan:live pairs, got {value:?}"
                ))
            })?;
            let orphan_manifests = orphans.parse::<i32>().map_err(|e| {
                std::io::Error::other(format!("invalid orphan count {orphans:?}: {e}"))
            })?;
            let live_manifests = live
                .parse::<i32>()
                .map_err(|e| std::io::Error::other(format!("invalid live count {live:?}: {e}")))?;
            if orphan_manifests < 0 || live_manifests < 0 {
                return Err(std::io::Error::other("scenario counts must be non-negative").into());
            }
            if orphan_manifests + live_manifests == 0 {
                return Err(
                    std::io::Error::other("a scenario must contain at least one manifest").into(),
                );
            }
            scenarios.push(Scenario {
                orphan_manifests,
                live_manifests,
            });
        }
        if scenarios.is_empty() {
            return Err(std::io::Error::other("GC_SCENARIOS must not be empty").into());
        }
        return Ok(scenarios);
    }

    Ok(parse_counts()?
        .into_iter()
        .map(|orphan_manifests| Scenario {
            orphan_manifests,
            live_manifests: (orphan_manifests / 100).max(10),
        })
        .collect())
}

fn parse_algorithms() -> Result<Vec<Algorithm>, Box<dyn Error>> {
    let mut algorithms = Vec::new();
    if env::var("GC_EXCLUDE_BASELINES").as_deref() != Ok("1") {
        algorithms.push(Algorithm::Current);
        if env::var("GC_INCLUDE_CTE").as_deref() != Ok("0") {
            algorithms.push(Algorithm::Batched);
        }
    }

    for (variable, aggregate_mode) in [
        ("GC_HYBRID_THRESHOLDS", AggregateMode::OwnedHashMap),
        ("GC_BORROWED_THRESHOLDS", AggregateMode::BorrowedHashMap),
        (
            "GC_SORTED_THRESHOLDS",
            AggregateMode::SortedBorrowed {
                occurrence_window: usize::MAX,
            },
        ),
    ] {
        let Ok(raw) = env::var(variable) else {
            continue;
        };
        for threshold in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let threshold = threshold.parse::<usize>().map_err(|e| {
                std::io::Error::other(format!("invalid hybrid threshold {threshold:?}: {e}"))
            })?;
            if ![2, 4, 8, 32, 500].contains(&threshold) {
                return Err(std::io::Error::other(
                    "hybrid thresholds are limited to the audited set: 2,4,8,32,500",
                )
                .into());
            }
            let algorithm = Algorithm::Hybrid {
                aggregate_threshold: threshold,
                aggregate_mode,
            };
            if !algorithms.contains(&algorithm) {
                algorithms.push(algorithm);
            }
        }
    }
    if let Ok(raw) = env::var("GC_SORTED_WINDOWS") {
        for window in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let occurrence_window = window.parse::<usize>().map_err(|e| {
                std::io::Error::other(format!("invalid sorted occurrence window {window:?}: {e}"))
            })?;
            if ![512, 1024, 2048, 4096].contains(&occurrence_window) {
                return Err(std::io::Error::other(
                    "sorted occurrence windows are limited to 512,1024,2048,4096",
                )
                .into());
            }
            algorithms.push(Algorithm::Hybrid {
                aggregate_threshold: 32,
                aggregate_mode: AggregateMode::SortedBorrowed { occurrence_window },
            });
        }
    }
    if algorithms.is_empty() {
        return Err(std::io::Error::other("at least one GC algorithm must be selected").into());
    }
    Ok(algorithms)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let database_url = env::var("DATABASE_URL").map_err(|_| {
        std::io::Error::other("DATABASE_URL must point at the throw-away benchmark database")
    })?;
    let scenarios = parse_scenarios()?;
    let algorithms = parse_algorithms()?;
    let chunks_per_manifest = parse_usize("GC_CHUNKS_PER_MANIFEST", 16)?;
    let shared_percent = parse_usize("GC_SHARED_PERCENT", 50)?;
    let shared_pool = parse_usize("GC_SHARED_POOL", 512)?;
    let warmups = parse_usize("GC_WARMUPS", 1)?;
    let samples = parse_usize("GC_SAMPLES", 5)?;

    if chunks_per_manifest < 2 || chunks_per_manifest > 128 {
        return Err(std::io::Error::other("GC_CHUNKS_PER_MANIFEST must be 2..=128").into());
    }
    if shared_percent == 0 || shared_percent >= 100 {
        return Err(std::io::Error::other("GC_SHARED_PERCENT must be 1..=99").into());
    }
    if shared_pool == 0 || samples == 0 {
        return Err(std::io::Error::other("shared pool and samples must be > 0").into());
    }

    // Probe one direct connection first so authentication/network failures are
    // reported with their real cause instead of the pool's generic timeout.
    let probe = sqlx::postgres::PgConnection::connect(&database_url).await?;
    probe.close().await?;

    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await?;

    println!("dedup GC phase-1 benchmark");
    println!("database          : {database_url}");
    println!("batch size        : {BATCH_SIZE}");
    println!("chunks/manifest   : {chunks_per_manifest}");
    println!("shared chunks     : {shared_percent}% (pool={shared_pool})");
    println!("duplicate control : every 10th manifest repeats one chunk");
    println!("warmups / samples : {warmups} / {samples}");
    println!(
        "algorithms        : {}",
        algorithms
            .iter()
            .map(|algorithm| algorithm.label())
            .collect::<Vec<_>>()
            .join(", ")
    );

    create_schema(&pool).await?;

    for scenario in scenarios {
        build_template(
            &pool,
            scenario.orphan_manifests,
            scenario.live_manifests,
            chunks_per_manifest as i32,
            shared_percent as i32,
            shared_pool as i32,
        )
        .await?;
        let stats = fixture_stats(&pool).await?;

        println!("\nfixture");
        println!("  orphan manifests : {}", stats.orphan_manifests);
        println!("  live controls    : {}", stats.live_manifests);
        println!("  unique blobs     : {}", stats.blob_rows);
        println!("  repeated-chunk manifests: {}", stats.duplicate_manifests);

        for _ in 0..warmups {
            for &algorithm in &algorithms {
                reset_fixture(&pool).await?;
                let warm = run_algorithm(&pool, algorithm).await?;
                validate(&pool, &stats, algorithm, &warm).await?;
            }
        }

        let mut outcomes: Vec<(Algorithm, Vec<RunOutcome>)> = algorithms
            .iter()
            .copied()
            .map(|algorithm| (algorithm, Vec::with_capacity(samples)))
            .collect();
        for sample in 0..samples {
            // Rotate the complete candidate set so no algorithm owns a
            // systematic hot/cold or checkpoint position. Rotation alone is
            // important for a two-way A/B: reversing after a one-step rotation
            // would accidentally restore the original order.
            let mut order = algorithms.clone();
            let order_len = order.len();
            order.rotate_left(sample % order_len);
            for algorithm in order {
                reset_fixture(&pool).await?;
                let outcome = run_algorithm(&pool, algorithm).await?;
                validate(&pool, &stats, algorithm, &outcome).await?;
                println!(
                    "  sample {:>2} {:>18}: {:>10.3} ms, {:>7} statements",
                    sample + 1,
                    algorithm.label(),
                    outcome.elapsed.as_secs_f64() * 1_000.0,
                    outcome.statements,
                );
                outcomes
                    .iter_mut()
                    .find(|(candidate, _)| *candidate == algorithm)
                    .expect("algorithm outcome bucket")
                    .1
                    .push(outcome);
            }
        }

        if let Some((_, current)) = outcomes
            .iter()
            .find(|(algorithm, _)| *algorithm == Algorithm::Current)
        {
            for (algorithm, candidate) in &outcomes {
                if *algorithm != Algorithm::Current {
                    print_comparison(current, *algorithm, candidate);
                }
            }
        } else {
            for (algorithm, outcome) in &outcomes {
                print_absolute(*algorithm, outcome);
            }
        }
    }

    pool.close().await;
    Ok(())
}

async fn create_schema(pool: &PgPool) -> Result<(), sqlx::Error> {
    // The runner uses a dedicated throw-away database, so owning the canonical
    // `storage` schema here cannot collide with a live OxiCloud instance.
    sqlx::query("DROP SCHEMA IF EXISTS storage CASCADE")
        .execute(pool)
        .await?;
    sqlx::query("DROP SCHEMA IF EXISTS perf_audit CASCADE")
        .execute(pool)
        .await?;
    sqlx::query("CREATE SCHEMA storage").execute(pool).await?;
    sqlx::query("CREATE SCHEMA perf_audit")
        .execute(pool)
        .await?;

    sqlx::query(
        "CREATE TABLE storage.blobs (
             hash VARCHAR(64) PRIMARY KEY,
             size BIGINT NOT NULL,
             ref_count INTEGER NOT NULL CHECK (ref_count >= 0),
             orphaned_at TIMESTAMPTZ
         )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX idx_blobs_gc_eligible
             ON storage.blobs (orphaned_at) WHERE ref_count = 0",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE storage.chunk_manifests (
             file_hash VARCHAR(64) PRIMARY KEY,
             chunk_hashes TEXT[] NOT NULL,
             chunk_sizes BIGINT[] NOT NULL,
             total_size BIGINT NOT NULL,
             chunk_count INTEGER NOT NULL,
             content_type TEXT,
             ref_count INTEGER NOT NULL CHECK (ref_count >= 0)
         )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX idx_chunk_manifests_ref_count_zero
             ON storage.chunk_manifests (file_hash) WHERE ref_count = 0",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE storage.files (
             blob_hash VARCHAR(64) NOT NULL,
             is_trashed BOOLEAN NOT NULL DEFAULT FALSE
         )",
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE INDEX idx_files_blob_hash ON storage.files (blob_hash)")
        .execute(pool)
        .await?;

    sqlx::query(
        "CREATE TABLE perf_audit.manifests (
             file_hash VARCHAR(64) PRIMARY KEY,
             chunk_hashes TEXT[] NOT NULL,
             chunk_sizes BIGINT[] NOT NULL,
             total_size BIGINT NOT NULL,
             chunk_count INTEGER NOT NULL,
             content_type TEXT,
             ref_count INTEGER NOT NULL
         )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE perf_audit.files (
             blob_hash VARCHAR(64) NOT NULL
         )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE perf_audit.blobs (
             hash VARCHAR(64) PRIMARY KEY,
             size BIGINT NOT NULL,
             ref_count INTEGER NOT NULL,
             expected_after INTEGER NOT NULL
         )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn build_template(
    pool: &PgPool,
    orphan_count: i32,
    live_count: i32,
    chunks: i32,
    shared_percent: i32,
    shared_pool: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query("TRUNCATE perf_audit.files, perf_audit.manifests, perf_audit.blobs")
        .execute(pool)
        .await?;
    let shared_slots = (chunks * shared_percent / 100).clamp(1, chunks - 1);

    insert_manifests(
        pool,
        "orphan",
        orphan_count,
        chunks,
        shared_slots,
        shared_pool,
        0,
    )
    .await?;
    insert_manifests(
        pool,
        "live",
        live_count,
        chunks,
        shared_slots,
        shared_pool,
        1,
    )
    .await?;

    sqlx::query(
        "INSERT INTO perf_audit.files (blob_hash)
         SELECT file_hash FROM perf_audit.manifests WHERE ref_count > 0",
    )
    .execute(pool)
    .await?;

    // Count one reference per DISTINCT chunk hash per manifest. This mirrors
    // `ChunkIngestOutcome::distinct_hashes()` in the production ingest path.
    sqlx::query(
        "INSERT INTO perf_audit.blobs (hash, size, ref_count, expected_after)
         SELECT d.chunk_hash,
                65536,
                COUNT(*)::integer,
                COUNT(*) FILTER (WHERE m.ref_count > 0)::integer
           FROM perf_audit.manifests m
           CROSS JOIN LATERAL (
               SELECT DISTINCT chunk_hash
                 FROM unnest(m.chunk_hashes) AS chunks(chunk_hash)
           ) d
          GROUP BY d.chunk_hash",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_manifests(
    pool: &PgPool,
    kind: &str,
    count: i32,
    chunks: i32,
    shared_slots: i32,
    shared_pool: i32,
    ref_count: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO perf_audit.manifests
             (file_hash, chunk_hashes, chunk_sizes, total_size,
              chunk_count, content_type, ref_count)
         SELECT $1 || '-file-' || i::text,
                ARRAY(
                    SELECT CASE
                        -- Duplicate control: ANY($1) updates this hash once,
                        -- not twice, in the current implementation.
                        WHEN i % 10 = 0 AND slot = $3 - 1
                            THEN 'shared-' || ((i * 17) % $5)::text
                        WHEN slot < $4
                            THEN 'shared-' || ((i * 17 + slot * 31) % $5)::text
                        ELSE 'unique-' || $1 || '-' || i::text || '-' || slot::text
                    END
                      FROM generate_series(0, $3 - 1) AS slots(slot)
                     ORDER BY slot
                ),
                array_fill(65536::bigint, ARRAY[$3]),
                $3::bigint * 65536,
                $3,
                'application/octet-stream',
                $6
           FROM generate_series(1, $2) AS manifests(i)",
    )
    .bind(kind)
    .bind(count)
    .bind(chunks)
    .bind(shared_slots)
    .bind(shared_pool)
    .bind(ref_count)
    .execute(pool)
    .await?;
    Ok(())
}

async fn fixture_stats(pool: &PgPool) -> Result<FixtureStats, sqlx::Error> {
    let row = sqlx::query(
        "SELECT
             COUNT(*) FILTER (WHERE ref_count = 0)::bigint AS orphans,
             COUNT(*) FILTER (WHERE ref_count > 0)::bigint AS live,
             (SELECT COUNT(*)::bigint FROM perf_audit.blobs) AS blobs,
             COUNT(*) FILTER (
                 WHERE cardinality(chunk_hashes)
                     > (SELECT COUNT(DISTINCT h) FROM unnest(chunk_hashes) AS x(h))
             )::bigint AS duplicate_manifests,
             COALESCE(SUM(total_size) FILTER (WHERE ref_count = 0), 0)::bigint
                 AS orphan_logical_bytes,
             COALESCE(MAX(chunk_count), 0)::bigint AS chunks_per_manifest
           FROM perf_audit.manifests",
    )
    .fetch_one(pool)
    .await?;
    Ok(FixtureStats {
        orphan_manifests: row.try_get("orphans")?,
        live_manifests: row.try_get("live")?,
        blob_rows: row.try_get("blobs")?,
        duplicate_manifests: row.try_get("duplicate_manifests")?,
        orphan_logical_bytes: row.try_get("orphan_logical_bytes")?,
        chunks_per_manifest: row.try_get("chunks_per_manifest")?,
    })
}

async fn reset_fixture(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("TRUNCATE storage.files, storage.chunk_manifests, storage.blobs")
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO storage.blobs (hash, size, ref_count, orphaned_at)
         SELECT hash, size, ref_count, NULL FROM perf_audit.blobs",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO storage.chunk_manifests
             (file_hash, chunk_hashes, chunk_sizes, total_size,
              chunk_count, content_type, ref_count)
         SELECT file_hash, chunk_hashes, chunk_sizes, total_size,
                chunk_count, content_type, ref_count
           FROM perf_audit.manifests",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO storage.files (blob_hash, is_trashed)
         SELECT blob_hash, FALSE FROM perf_audit.files",
    )
    .execute(pool)
    .await?;
    sqlx::query("ANALYZE storage.files, storage.chunk_manifests, storage.blobs")
        .execute(pool)
        .await?;
    Ok(())
}

async fn run_algorithm(pool: &PgPool, algorithm: Algorithm) -> Result<RunOutcome, sqlx::Error> {
    let started = Instant::now();
    let mut statements = 0u64;
    let mut deleted_manifests = 0u64;
    let mut logical_bytes = 0u64;
    let mut updated_blob_rows = 0u64;
    let mut checksum = 0u64;

    loop {
        match algorithm {
            Algorithm::Current => {
                let batch: Vec<(String, Vec<String>, i64)> = sqlx::query_as(CURRENT_DELETE)
                    .bind(BATCH_SIZE)
                    .fetch_all(pool)
                    .await?;
                statements += 1;
                if batch.is_empty() {
                    break;
                }
                for (file_hash, chunk_hashes, size) in batch {
                    let affected = sqlx::query(CURRENT_UPDATE)
                        .bind(&chunk_hashes)
                        .execute(pool)
                        .await?
                        .rows_affected();
                    statements += 1;
                    deleted_manifests += 1;
                    logical_bytes += size as u64;
                    updated_blob_rows += affected;
                    checksum = checksum.wrapping_add(file_hash.len() as u64);
                }
            }
            Algorithm::Batched => {
                let batch: Vec<(String, Vec<String>, i64, i64)> = sqlx::query_as(BATCHED_CTE)
                    .bind(BATCH_SIZE)
                    .fetch_all(pool)
                    .await?;
                statements += 1;
                if batch.is_empty() {
                    break;
                }
                let batch_updated = batch[0].3 as u64;
                updated_blob_rows += batch_updated;
                for (file_hash, _chunk_hashes, size, _) in batch {
                    deleted_manifests += 1;
                    logical_bytes += size as u64;
                    checksum = checksum.wrapping_add(file_hash.len() as u64);
                }
            }
            Algorithm::Hybrid {
                aggregate_threshold,
                aggregate_mode,
            } => {
                let batch: Vec<(String, Vec<String>, i64)> = sqlx::query_as(CURRENT_DELETE)
                    .bind(BATCH_SIZE)
                    .fetch_all(pool)
                    .await?;
                statements += 1;
                if batch.is_empty() {
                    break;
                }

                if batch.len() < aggregate_threshold {
                    for (_, chunk_hashes, _) in &batch {
                        let affected = sqlx::query(CURRENT_UPDATE)
                            .bind(chunk_hashes)
                            .execute(pool)
                            .await?
                            .rows_affected();
                        statements += 1;
                        updated_blob_rows += affected;
                    }
                } else {
                    match aggregate_mode {
                        AggregateMode::OwnedHashMap | AggregateMode::BorrowedHashMap => {
                            let mut decrements = HashMap::<&str, i32>::new();
                            for (_, chunk_hashes, _) in &batch {
                                let distinct: HashSet<&str> =
                                    chunk_hashes.iter().map(String::as_str).collect();
                                for hash in distinct {
                                    *decrements.entry(hash).or_default() += 1;
                                }
                            }
                            if aggregate_mode == AggregateMode::BorrowedHashMap {
                                let (hashes, decrement_by): (Vec<&str>, Vec<i32>) =
                                    decrements.into_iter().unzip();
                                updated_blob_rows += sqlx::query(AGGREGATED_UPDATE)
                                    .bind(&hashes)
                                    .bind(&decrement_by)
                                    .execute(pool)
                                    .await?
                                    .rows_affected();
                            } else {
                                let (hashes, decrement_by): (Vec<String>, Vec<i32>) = decrements
                                    .into_iter()
                                    .map(|(hash, decrement)| (hash.to_owned(), decrement))
                                    .unzip();
                                updated_blob_rows += sqlx::query(AGGREGATED_UPDATE)
                                    .bind(&hashes)
                                    .bind(&decrement_by)
                                    .execute(pool)
                                    .await?
                                    .rows_affected();
                            }
                        }
                        AggregateMode::SortedBorrowed { occurrence_window } => {
                            let (affected, update_statements) =
                                run_sorted_updates(pool, &batch, occurrence_window).await?;
                            updated_blob_rows += affected;
                            // The common one-window case is accounted below.
                            statements += update_statements - 1;
                        }
                    }
                    statements += 1;
                }

                for (file_hash, _chunk_hashes, size) in batch {
                    deleted_manifests += 1;
                    logical_bytes += size as u64;
                    checksum = checksum.wrapping_add(file_hash.len() as u64);
                }
            }
        }
    }

    Ok(RunOutcome {
        elapsed: started.elapsed(),
        statements,
        deleted_manifests,
        logical_bytes,
        updated_blob_rows,
        checksum,
    })
}

async fn run_sorted_updates(
    pool: &PgPool,
    batch: &[(String, Vec<String>, i64)],
    occurrence_window: usize,
) -> Result<(u64, u64), sqlx::Error> {
    let mut first = 0usize;
    let mut affected = 0u64;
    let mut statements = 0u64;
    while first < batch.len() {
        let mut end = first;
        let mut occurrences = 0usize;
        while end < batch.len() {
            let next = batch[end].1.len();
            if end > first && occurrences.saturating_add(next) > occurrence_window {
                break;
            }
            occurrences = occurrences.saturating_add(next);
            end += 1;
            if occurrences >= occurrence_window {
                break;
            }
        }

        let group = &batch[first..end];
        if group.len() == 1 {
            affected += sqlx::query(CURRENT_UPDATE)
                .bind(&group[0].1)
                .execute(pool)
                .await?
                .rows_affected();
        } else {
            let mut all_hashes = Vec::<&str>::with_capacity(occurrences);
            let mut per_manifest = Vec::<&str>::new();
            for (_, chunk_hashes, _) in group {
                per_manifest.clear();
                per_manifest.extend(chunk_hashes.iter().map(String::as_str));
                per_manifest.sort_unstable();
                per_manifest.dedup();
                all_hashes.extend_from_slice(&per_manifest);
            }
            all_hashes.sort_unstable();

            let mut hashes = Vec::<&str>::with_capacity(all_hashes.len());
            let mut decrement_by = Vec::<i32>::with_capacity(all_hashes.len());
            for hash in all_hashes {
                if hashes.last().copied() == Some(hash) {
                    *decrement_by.last_mut().expect("count for existing hash") += 1;
                } else {
                    hashes.push(hash);
                    decrement_by.push(1);
                }
            }
            affected += sqlx::query(AGGREGATED_UPDATE)
                .bind(&hashes)
                .bind(&decrement_by)
                .execute(pool)
                .await?
                .rows_affected();
        }
        statements += 1;
        first = end;
    }
    Ok((affected, statements))
}

async fn validate(
    pool: &PgPool,
    stats: &FixtureStats,
    algorithm: Algorithm,
    outcome: &RunOutcome,
) -> Result<(), Box<dyn Error>> {
    let row = sqlx::query(
        "SELECT
             (SELECT COUNT(*)::bigint
                FROM storage.chunk_manifests m
               WHERE m.ref_count <= 0
                  OR NOT EXISTS (
                      SELECT 1 FROM storage.files f
                       WHERE f.blob_hash = m.file_hash
                  )) AS remaining_collectible,
             (SELECT COUNT(*)::bigint FROM storage.chunk_manifests) AS remaining_live,
             (SELECT COUNT(*)::bigint
                FROM storage.blobs b
                JOIN perf_audit.blobs expected USING (hash)
               WHERE b.ref_count <> expected.expected_after
                  OR (expected.expected_after = 0 AND b.orphaned_at IS NULL)
                  OR (expected.expected_after > 0 AND b.orphaned_at IS NOT NULL)
             ) AS ref_mismatches,
             (SELECT COUNT(*)::bigint FROM storage.blobs WHERE ref_count < 0) AS underflows",
    )
    .fetch_one(pool)
    .await?;

    let remaining_collectible: i64 = row.try_get("remaining_collectible")?;
    let remaining_live: i64 = row.try_get("remaining_live")?;
    let ref_mismatches: i64 = row.try_get("ref_mismatches")?;
    let underflows: i64 = row.try_get("underflows")?;
    let expected_bytes = stats.orphan_logical_bytes as u64;

    let successful_batches = (stats.orphan_manifests as u64).div_ceil(BATCH_SIZE as u64);
    let expected_statements = successful_batches
        + 1
        + match algorithm {
            // The historical implementation issued one UPDATE per returned
            // manifest. The CTE has no per-row statement.
            Algorithm::Current => stats.orphan_manifests as u64,
            Algorithm::Batched => 0,
            Algorithm::Hybrid {
                aggregate_threshold,
                aggregate_mode,
            } => {
                let mut remaining = stats.orphan_manifests as u64;
                let mut updates = 0;
                while remaining > 0 {
                    let batch = remaining.min(BATCH_SIZE as u64);
                    updates += if batch < aggregate_threshold as u64 {
                        batch
                    } else if let AggregateMode::SortedBorrowed { occurrence_window } =
                        aggregate_mode
                    {
                        let manifests_per_window = if occurrence_window == usize::MAX {
                            BATCH_SIZE as u64
                        } else {
                            (occurrence_window as u64 / stats.chunks_per_manifest as u64).max(1)
                        };
                        batch.div_ceil(manifests_per_window)
                    } else {
                        1
                    };
                    remaining -= batch;
                }
                updates
            }
        };

    if remaining_collectible != 0
        || remaining_live != stats.live_manifests
        || ref_mismatches != 0
        || underflows != 0
        || outcome.deleted_manifests != stats.orphan_manifests as u64
        || outcome.logical_bytes != expected_bytes
        || (stats.orphan_manifests > 0 && outcome.checksum == 0)
        || (stats.orphan_manifests > 0 && outcome.updated_blob_rows == 0)
        || outcome.statements != expected_statements
    {
        return Err(std::io::Error::other(format!(
            "correctness failure: remaining_collectible={remaining_collectible}, \
             remaining_live={remaining_live}/{}, ref_mismatches={ref_mismatches}, \
             underflows={underflows}, deleted={}/{}, bytes={}/{expected_bytes}, \
             updated_blob_rows={}, checksum={}, statements={}/{expected_statements}",
            stats.live_manifests,
            outcome.deleted_manifests,
            stats.orphan_manifests,
            outcome.logical_bytes,
            outcome.updated_blob_rows,
            outcome.checksum,
            outcome.statements,
        ))
        .into());
    }
    Ok(())
}

fn percentile_ms(outcomes: &[RunOutcome], percentile: f64) -> f64 {
    let mut values: Vec<f64> = outcomes
        .iter()
        .map(|o| o.elapsed.as_secs_f64() * 1_000.0)
        .collect();
    values.sort_by(f64::total_cmp);
    let index = ((values.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(values.len() - 1);
    values[index]
}

fn print_comparison(
    current: &[RunOutcome],
    candidate_algorithm: Algorithm,
    candidate: &[RunOutcome],
) {
    let cur_median = percentile_ms(current, 0.50);
    let cur_p95 = percentile_ms(current, 0.95);
    let new_median = percentile_ms(candidate, 0.50);
    let new_p95 = percentile_ms(candidate, 0.95);
    let speedup = cur_median / new_median;
    let reduction = (1.0 - new_median / cur_median) * 100.0;
    let current_statements = current[0].statements;
    let candidate_statements = candidate[0].statements;

    println!("comparison: {}", candidate_algorithm.label());
    println!(
        "  {:>18}: median={:>10.3} ms p95={:>10.3} ms statements={current_statements}",
        Algorithm::Current.label(),
        cur_median,
        cur_p95,
    );
    println!(
        "  {:>18}: median={:>10.3} ms p95={:>10.3} ms statements={candidate_statements}",
        candidate_algorithm.label(),
        new_median,
        new_p95,
    );
    println!("  median delta      : {reduction:.2}% faster ({speedup:.2}x)");
    println!(
        "  statement delta   : {:.2}% fewer ({current_statements} -> {candidate_statements})",
        (1.0 - candidate_statements as f64 / current_statements as f64) * 100.0,
    );
    println!("  correctness       : PASS for every warmup and measured sample");
}

fn print_absolute(algorithm: Algorithm, outcomes: &[RunOutcome]) {
    println!("summary: {}", algorithm.label());
    println!(
        "  median={:.3} ms p95={:.3} ms statements={}",
        percentile_ms(outcomes, 0.50),
        percentile_ms(outcomes, 0.95),
        outcomes[0].statements,
    );
    println!("  correctness       : PASS for every warmup and measured sample");
}
