//! PostgreSQL-backed A/B/C for online manifest integrity verification.
//!
//! This is an audit-only executable. It compares the historical serial
//! `fetch_all`, the current owned-window `fetch_all`, and a bounded online
//! SQLx `.fetch` design. All modes issue one manifest query and, when `full`
//! is requested, the same one-query streamed phase 2.

use foldhash::quality::RandomState;
use futures::TryStreamExt;
use futures::stream::{self, StreamExt};
use sqlx::postgres::{PgConnection, PgPoolOptions};
use sqlx::{Connection, PgPool, Row};
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::future::Future;
use std::hint::black_box;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const WINDOW: usize = 256;
const CURRENT_CONCURRENCY: usize = 16;
const STREAMING_CONCURRENCY: usize = 8;
const SERIAL_FAST_PATH_OCCURRENCES: usize = 4;
const PHASE_TWO_CONCURRENCY: usize = 16;
const PREFETCH_ROWS: usize = 16;
type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type OwnedSizes = HashMap<String, Option<u64>, RandomState>;
type ManifestRow = (i64, String, Vec<String>, Vec<i64>, i64);

const MANIFEST_QUERY: &str = r#"
SELECT ordinal, file_hash, chunk_hashes, chunk_sizes, total_size
  FROM perf_integrity.manifests
 WHERE scenario = $1
 ORDER BY ordinal
"#;

const BLOB_QUERY: &str = r#"
SELECT hash, size
  FROM perf_integrity.blobs
 WHERE scenario = $1
 ORDER BY ordinal
"#;

const SEED_SQL: &str = r#"
DROP SCHEMA IF EXISTS perf_integrity CASCADE;
CREATE SCHEMA perf_integrity;
CREATE TABLE perf_integrity.manifests (
    scenario text NOT NULL,
    ordinal bigint NOT NULL,
    file_hash text NOT NULL,
    chunk_hashes text[] NOT NULL,
    chunk_sizes bigint[] NOT NULL,
    total_size bigint NOT NULL,
    PRIMARY KEY (scenario, ordinal)
);
CREATE TABLE perf_integrity.blobs (
    scenario text NOT NULL,
    ordinal bigint NOT NULL,
    hash text NOT NULL,
    size bigint NOT NULL,
    PRIMARY KEY (scenario, ordinal)
);

INSERT INTO perf_integrity.manifests VALUES
('one', 0, 'file-one', ARRAY[md5('0') || md5('0x')], ARRAY[256::bigint], 256),
('four', 0, 'file-four',
 ARRAY(SELECT md5(i::text) || md5(i::text || 'x') FROM generate_series(0, 3) AS g(i)),
 ARRAY[256::bigint, 256, 256, 256], 1024);

WITH chunks AS (
    SELECT scenario, manifest, chunk,
           md5(hash_index::text) || md5(hash_index::text || 'x') AS hash
      FROM (
          SELECT 'shared'::text AS scenario, m AS manifest, c AS chunk,
                 ((m * 8 + c) % 32)::bigint AS hash_index
            FROM generate_series(0, 63) AS manifests(m)
            CROSS JOIN generate_series(0, 7) AS chunks(c)
          UNION ALL
          SELECT 'unique'::text, m, c, (m * 8 + c)::bigint
            FROM generate_series(0, 31) AS manifests(m)
            CROSS JOIN generate_series(0, 7) AS chunks(c)
          UNION ALL
          SELECT 'large'::text, m, c, (m * 250 + c)::bigint
            FROM generate_series(0, 999) AS manifests(m)
            CROSS JOIN generate_series(0, 249) AS chunks(c)
          UNION ALL
          SELECT 'large_manifest'::text, 0, c, c::bigint
            FROM generate_series(0, 1023) AS chunks(c)
      ) AS source
), aggregated AS (
    SELECT scenario, manifest,
           array_agg(hash ORDER BY chunk) AS hashes,
           array_agg(256::bigint ORDER BY chunk) AS sizes,
           COUNT(*)::bigint * 256 AS total_size
      FROM chunks
     GROUP BY scenario, manifest
)
INSERT INTO perf_integrity.manifests
SELECT scenario, manifest, 'file-' || scenario || '-' || manifest,
       hashes, sizes, total_size
  FROM aggregated;

INSERT INTO perf_integrity.manifests VALUES
('semantics', 0, 'sum-mismatch-file', ARRAY['wrong-shared', 'wrong-shared'],
 ARRAY[256::bigint, 257], 1),
('semantics', 1, 'missing-file', ARRAY['missing-shared', 'missing-shared'],
 ARRAY[256::bigint, 256], 512),
('semantics', 2, 'valid-file',
 ARRAY[md5('semantics-0') || md5('semantics-0x'), md5('semantics-1') || md5('semantics-1x')],
 ARRAY[256::bigint, 256], 512),
('semantics', 3, 'malformed-file', ARRAY['missing-must-not-be-queried'],
 ARRAY[]::bigint[], 0);

WITH distinct_hashes AS (
    SELECT scenario, hash
      FROM perf_integrity.manifests
      CROSS JOIN LATERAL unnest(chunk_hashes) AS u(hash)
     WHERE hash <> 'missing-must-not-be-queried'
     GROUP BY scenario, hash
), numbered AS (
    SELECT scenario, hash,
           row_number() OVER (PARTITION BY scenario ORDER BY hash) - 1 AS ordinal
      FROM distinct_hashes
)
INSERT INTO perf_integrity.blobs
SELECT scenario, ordinal, hash, 256 FROM numbered;

ANALYZE perf_integrity.manifests;
ANALYZE perf_integrity.blobs;
"#;

const SEED_SMOKE_SQL: &str = r#"
DROP SCHEMA IF EXISTS perf_integrity CASCADE;
CREATE SCHEMA perf_integrity;
CREATE TABLE perf_integrity.manifests (
    scenario text NOT NULL,
    ordinal bigint NOT NULL,
    file_hash text NOT NULL,
    chunk_hashes text[] NOT NULL,
    chunk_sizes bigint[] NOT NULL,
    total_size bigint NOT NULL,
    PRIMARY KEY (scenario, ordinal)
);
CREATE TABLE perf_integrity.blobs (
    scenario text NOT NULL,
    ordinal bigint NOT NULL,
    hash text NOT NULL,
    size bigint NOT NULL,
    PRIMARY KEY (scenario, ordinal)
);
INSERT INTO perf_integrity.manifests VALUES
('one', 0, 'file-one', ARRAY[md5('0') || md5('0x')], ARRAY[256::bigint], 256);
INSERT INTO perf_integrity.blobs
SELECT 'one', 0, md5('0') || md5('0x'), 256;
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Historical,
    MaterializedOwned,
    StreamingSorted,
    StreamingPrefetch,
}

impl Mode {
    fn parse(value: &str) -> Self {
        match value {
            "historical" => Self::Historical,
            "materialized" => Self::MaterializedOwned,
            "streaming" => Self::StreamingSorted,
            "prefetch" => Self::StreamingPrefetch,
            _ => panic!("mode must be historical, materialized, streaming, or prefetch"),
        }
    }
}

#[derive(Default)]
struct ModelBackend {
    calls: AtomicUsize,
}

impl ModelBackend {
    fn blob_size<'a>(&'a self, hash: &'a str) -> BoxFut<'a, Option<u64>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if hash.starts_with("missing-") {
                None
            } else if hash.starts_with("wrong-") {
                Some(999)
            } else {
                Some(256)
            }
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
struct Outcome {
    phase_elapsed: Duration,
    full_elapsed: Duration,
    issues: Vec<String>,
    phase_calls: usize,
    full_calls: usize,
    manifest_rows: usize,
    queries: usize,
    held_connection_while_streaming: bool,
}

#[inline]
fn label(value: &str) -> &str {
    &value[..value.len().min(12)]
}

#[inline]
fn valid_occurrences(row: &ManifestRow) -> usize {
    if row.2.len() == row.3.len() {
        row.2.len()
    } else {
        0
    }
}

fn uses_serial_fast_path(rows: &[ManifestRow]) -> bool {
    if rows.len() == 1 {
        return rows[0].2.len() != rows[0].3.len()
            || rows[0].2.len() <= SERIAL_FAST_PATH_OCCURRENCES;
    }
    let mut occurrences = 0usize;
    for row in rows {
        occurrences = occurrences.saturating_add(valid_occurrences(row));
        if occurrences > SERIAL_FAST_PATH_OCCURRENCES {
            return false;
        }
    }
    true
}

async fn serial_rows(rows: &[ManifestRow], backend: &ModelBackend) -> Vec<String> {
    let mut issues = Vec::new();
    for (_, file_hash, hashes, expected_sizes, total_size) in rows {
        let file_label = label(file_hash);
        if hashes.len() != expected_sizes.len() {
            issues.push(format!(
                "Manifest {file_label}: chunk_hashes/chunk_sizes length mismatch"
            ));
            continue;
        }
        let sum: i64 = expected_sizes.iter().sum();
        if sum != *total_size {
            issues.push(format!(
                "Manifest {file_label}: total_size {total_size} != sum of chunk_sizes {sum}"
            ));
        }
        for (index, hash) in hashes.iter().enumerate() {
            let chunk_label = label(hash);
            match backend.blob_size(hash).await {
                Some(actual) if actual != expected_sizes[index] as u64 => issues.push(format!(
                    "Manifest {file_label} chunk {chunk_label}: size mismatch (expected {}, actual {actual})",
                    expected_sizes[index]
                )),
                None => issues.push(format!(
                    "Manifest {file_label} chunk {chunk_label}: missing in backend"
                )),
                Some(_) => {}
            }
        }
    }
    issues
}

fn replay_with<F>(rows: &[ManifestRow], mut size_of: F) -> Vec<String>
where
    F: FnMut(&str) -> Option<u64>,
{
    let mut issues = Vec::new();
    for (_, file_hash, hashes, expected_sizes, total_size) in rows {
        let file_label = label(file_hash);
        if hashes.len() != expected_sizes.len() {
            issues.push(format!(
                "Manifest {file_label}: chunk_hashes/chunk_sizes length mismatch"
            ));
            continue;
        }
        let sum: i64 = expected_sizes.iter().sum();
        if sum != *total_size {
            issues.push(format!(
                "Manifest {file_label}: total_size {total_size} != sum of chunk_sizes {sum}"
            ));
        }
        for (index, hash) in hashes.iter().enumerate() {
            let chunk_label = label(hash);
            match size_of(hash) {
                Some(actual) if actual != expected_sizes[index] as u64 => issues.push(format!(
                    "Manifest {file_label} chunk {chunk_label}: size mismatch (expected {}, actual {actual})",
                    expected_sizes[index]
                )),
                None => issues.push(format!(
                    "Manifest {file_label} chunk {chunk_label}: missing in backend"
                )),
                Some(_) => {}
            }
        }
    }
    issues
}

async fn owned_batch(rows: &[ManifestRow], backend: &ModelBackend) -> Vec<String> {
    let mut sizes = OwnedSizes::default();
    for row in rows {
        if row.2.len() == row.3.len() {
            for hash in &row.2 {
                sizes.entry(hash.clone()).or_insert(None);
            }
        }
    }
    let hashes: Vec<String> = sizes.keys().cloned().collect();
    let sizes = stream::iter(hashes)
        .map(|hash| async move {
            let size = backend.blob_size(&hash).await;
            (hash, size)
        })
        .buffer_unordered(CURRENT_CONCURRENCY)
        .fold(sizes, |mut sizes, (hash, size)| async move {
            sizes.insert(hash, size);
            sizes
        })
        .await;
    replay_with(rows, |hash| sizes.get(hash).copied().flatten())
}

async fn sorted_batch(rows: &[ManifestRow], backend: &ModelBackend) -> Vec<String> {
    let mut hashes = Vec::new();
    for row in rows {
        if row.2.len() == row.3.len() {
            hashes.extend(row.2.iter().map(String::as_str));
        }
    }
    hashes.sort_unstable();
    hashes.dedup();
    let values = vec![None; hashes.len()];
    let values = stream::iter(hashes.iter().copied().enumerate())
        .map(|(index, hash)| async move {
            let value = backend.blob_size(hash).await;
            (index, value)
        })
        .buffer_unordered(STREAMING_CONCURRENCY)
        .fold(values, |mut values, (index, value)| async move {
            values[index] = value;
            values
        })
        .await;
    replay_with(rows, |hash| {
        hashes
            .binary_search(&hash)
            .ok()
            .and_then(|index| values[index])
    })
}

async fn large_row(row: &ManifestRow, backend: &ModelBackend, mode: Mode) -> Vec<String> {
    let (_, file_hash, hashes, expected_sizes, total_size) = row;
    let file_label = label(file_hash);
    let mut issues = Vec::new();
    let sum: i64 = expected_sizes.iter().sum();
    if sum != *total_size {
        issues.push(format!(
            "Manifest {file_label}: total_size {total_size} != sum of chunk_sizes {sum}"
        ));
    }
    for offset in (0..hashes.len()).step_by(WINDOW) {
        let end = (offset + WINDOW).min(hashes.len());
        let slice_row = (
            row.0,
            file_hash.clone(),
            hashes[offset..end].to_vec(),
            expected_sizes[offset..end].to_vec(),
            expected_sizes[offset..end].iter().sum(),
        );
        let slice = std::slice::from_ref(&slice_row);
        let mut slice_issues = match mode {
            Mode::MaterializedOwned => owned_batch(slice, backend).await,
            Mode::StreamingSorted | Mode::StreamingPrefetch => sorted_batch(slice, backend).await,
            Mode::Historical => unreachable!(),
        };
        slice_issues.retain(|issue| !issue.contains("sum of chunk_sizes"));
        issues.extend(slice_issues);
    }
    issues
}

async fn process_materialized_windowed(
    rows: &[ManifestRow],
    backend: &ModelBackend,
    mode: Mode,
) -> Vec<String> {
    let mut issues = Vec::new();
    let mut start = 0usize;
    while start < rows.len() {
        let next = valid_occurrences(&rows[start]);
        if next > WINDOW {
            issues.extend(large_row(&rows[start], backend, mode).await);
            start += 1;
            continue;
        }
        let mut occurrences = 0usize;
        let mut end = start;
        while end < rows.len() {
            let next = valid_occurrences(&rows[end]);
            if next > WINDOW || (occurrences > 0 && occurrences + next > WINDOW) {
                break;
            }
            occurrences += next;
            end += 1;
        }
        debug_assert!(end > start);
        issues.extend(match mode {
            Mode::MaterializedOwned => owned_batch(&rows[start..end], backend).await,
            Mode::StreamingSorted | Mode::StreamingPrefetch => {
                sorted_batch(&rows[start..end], backend).await
            }
            Mode::Historical => unreachable!(),
        });
        start = end;
    }
    issues
}

struct WindowProcessor<'a> {
    backend: &'a ModelBackend,
    mode: Mode,
    rows: Vec<ManifestRow>,
    occurrences: usize,
    issues: Vec<String>,
}

impl<'a> WindowProcessor<'a> {
    fn new(backend: &'a ModelBackend, mode: Mode) -> Self {
        Self {
            backend,
            mode,
            rows: Vec::new(),
            occurrences: 0,
            issues: Vec::new(),
        }
    }

    async fn flush(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut self.rows);
        let issues = match self.mode {
            Mode::MaterializedOwned => owned_batch(&rows, self.backend).await,
            Mode::StreamingSorted | Mode::StreamingPrefetch => {
                sorted_batch(&rows, self.backend).await
            }
            Mode::Historical => unreachable!(),
        };
        self.issues.extend(issues);
        self.occurrences = 0;
    }

    async fn push(&mut self, row: ManifestRow) {
        let next = valid_occurrences(&row);
        if next > WINDOW {
            self.flush().await;
            self.issues
                .extend(large_row(&row, self.backend, self.mode).await);
            return;
        }
        if self.occurrences > 0 && self.occurrences + next > WINDOW {
            self.flush().await;
        }
        self.occurrences += next;
        self.rows.push(row);
    }

    async fn finish(mut self) -> Vec<String> {
        self.flush().await;
        self.issues
    }
}

fn decode(row: sqlx::postgres::PgRow) -> Result<ManifestRow, sqlx::Error> {
    Ok((
        row.try_get("ordinal")?,
        row.try_get("file_hash")?,
        row.try_get("chunk_hashes")?,
        row.try_get("chunk_sizes")?,
        row.try_get("total_size")?,
    ))
}

async fn phase_one_materialized(
    pool: &PgPool,
    scenario: &str,
    backend: &ModelBackend,
    mode: Mode,
) -> Result<(Vec<String>, Vec<ManifestRow>), sqlx::Error> {
    let tuples: Vec<ManifestRow> = sqlx::query_as(MANIFEST_QUERY)
        .bind(scenario)
        .fetch_all(pool)
        .await?;
    let issues = if mode == Mode::Historical || uses_serial_fast_path(&tuples) {
        serial_rows(&tuples, backend).await
    } else {
        process_materialized_windowed(&tuples, backend, mode).await
    };
    Ok((issues, tuples))
}

async fn phase_one_streaming(
    pool: &PgPool,
    scenario: &str,
    backend: &ModelBackend,
) -> Result<(Vec<String>, usize, bool), sqlx::Error> {
    let mut rows = sqlx::query(MANIFEST_QUERY).bind(scenario).fetch(pool);
    let mut initial = Vec::new();
    let mut occurrences = 0usize;
    let mut row_count = 0usize;
    let mut held_connection = false;
    let mut reached_eof = false;

    while occurrences <= SERIAL_FAST_PATH_OCCURRENCES {
        let Some(row) = rows.try_next().await? else {
            reached_eof = true;
            break;
        };
        held_connection |= pool.num_idle() == 0;
        let row = decode(row)?;
        occurrences = occurrences.saturating_add(valid_occurrences(&row));
        row_count += 1;
        initial.push(row);
    }

    if reached_eof {
        debug_assert!(uses_serial_fast_path(&initial));
        drop(rows);
        return Ok((
            serial_rows(&initial, backend).await,
            row_count,
            held_connection,
        ));
    }

    let mut processor = WindowProcessor::new(backend, Mode::StreamingSorted);
    for row in initial {
        processor.push(row).await;
    }
    while let Some(row) = rows.try_next().await? {
        held_connection |= pool.num_idle() == 0;
        processor.push(decode(row)?).await;
        row_count += 1;
    }
    drop(rows);
    Ok((processor.finish().await, row_count, held_connection))
}

async fn phase_one_streaming_prefetch(
    pool: &PgPool,
    scenario: &str,
    backend: &ModelBackend,
) -> Result<(Vec<String>, usize, bool), sqlx::Error> {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(PREFETCH_ROWS);
    let producer_pool = pool.clone();
    let producer_scenario = scenario.to_owned();
    let connection_held = Arc::new(AtomicBool::new(false));
    let producer_held = connection_held.clone();
    let producer = tokio::spawn(async move {
        let mut rows = sqlx::query(MANIFEST_QUERY)
            .bind(producer_scenario)
            .fetch(&producer_pool);
        while let Some(row) = rows.try_next().await? {
            producer_held.fetch_or(producer_pool.num_idle() == 0, Ordering::Relaxed);
            if sender.send(decode(row)?).await.is_err() {
                break;
            }
        }
        Ok::<(), sqlx::Error>(())
    });

    let mut initial = Vec::new();
    let mut occurrences = 0usize;
    let mut row_count = 0usize;
    let mut reached_eof = false;
    while occurrences <= SERIAL_FAST_PATH_OCCURRENCES {
        let Some(row) = receiver.recv().await else {
            reached_eof = true;
            break;
        };
        occurrences = occurrences.saturating_add(valid_occurrences(&row));
        row_count += 1;
        initial.push(row);
    }

    if reached_eof {
        producer
            .await
            .expect("manifest prefetch producer panicked")?;
        debug_assert!(uses_serial_fast_path(&initial));
        return Ok((
            serial_rows(&initial, backend).await,
            row_count,
            connection_held.load(Ordering::Relaxed),
        ));
    }

    let mut processor = WindowProcessor::new(backend, Mode::StreamingPrefetch);
    for row in initial {
        processor.push(row).await;
    }
    while let Some(row) = receiver.recv().await {
        processor.push(row).await;
        row_count += 1;
    }
    producer
        .await
        .expect("manifest prefetch producer panicked")?;
    Ok((
        processor.finish().await,
        row_count,
        connection_held.load(Ordering::Relaxed),
    ))
}

async fn phase_two(
    pool: &PgPool,
    scenario: &str,
    backend: &ModelBackend,
) -> Result<Vec<String>, sqlx::Error> {
    let mut rows = sqlx::query(BLOB_QUERY).bind(scenario).fetch(pool);
    let mut issues = Vec::new();
    let mut batch = Vec::with_capacity(PHASE_TWO_CONCURRENCY);
    loop {
        let next = rows.try_next().await?;
        let done = next.is_none();
        if let Some(row) = next {
            batch.push((
                row.try_get::<String, _>("hash")?,
                row.try_get::<i64, _>("size")?,
            ));
        }
        if batch.len() == PHASE_TWO_CONCURRENCY || (done && !batch.is_empty()) {
            let current = std::mem::replace(&mut batch, Vec::with_capacity(PHASE_TWO_CONCURRENCY));
            let mut batch_issues: Vec<String> = stream::iter(current)
                .map(|(hash, expected)| async move {
                    match backend.blob_size(&hash).await {
                        Some(actual) if actual != expected as u64 => Some(format!(
                            "{hash}: size mismatch (expected: {expected}, actual: {actual})"
                        )),
                        None => Some(format!("{hash}: blob missing in backend")),
                        Some(_) => None,
                    }
                })
                .buffer_unordered(PHASE_TWO_CONCURRENCY)
                .filter_map(async move |issue| issue)
                .collect()
                .await;
            issues.append(&mut batch_issues);
        }
        if done {
            break;
        }
    }
    Ok(issues)
}

async fn run(
    pool: &PgPool,
    mode: Mode,
    scenario: &str,
    full: bool,
) -> Result<Outcome, sqlx::Error> {
    let backend = Arc::new(ModelBackend::default());
    let start = Instant::now();
    let (mut issues, manifest_rows, held, retained_rows) = match mode {
        Mode::Historical | Mode::MaterializedOwned => {
            let (issues, rows) = phase_one_materialized(pool, scenario, &backend, mode).await?;
            let count = rows.len();
            (issues, count, false, Some(rows))
        }
        Mode::StreamingSorted => {
            let (issues, count, held) = phase_one_streaming(pool, scenario, &backend).await?;
            (issues, count, held, None)
        }
        Mode::StreamingPrefetch => {
            let (issues, count, held) =
                phase_one_streaming_prefetch(pool, scenario, &backend).await?;
            (issues, count, held, None)
        }
    };
    let phase_elapsed = start.elapsed();
    let phase_calls = backend.calls();
    if full {
        issues.extend(phase_two(pool, scenario, &backend).await?);
    }
    let full_elapsed = start.elapsed();
    let full_calls = backend.calls();
    black_box(&issues);
    black_box(&retained_rows);
    Ok(Outcome {
        phase_elapsed,
        full_elapsed,
        issues,
        phase_calls,
        full_calls,
        manifest_rows,
        queries: 1 + usize::from(full),
        held_connection_while_streaming: held,
    })
}

fn checksum(issues: &[String]) -> u64 {
    issues
        .iter()
        .flat_map(|issue| issue.bytes())
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x1000_0000_01b3)
        })
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().collect();
    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL is required");

    if args
        .get(1)
        .is_some_and(|value| value == "seed" || value == "seed-smoke")
    {
        let mut connection = PgConnection::connect(&database_url).await?;
        let sql = if args.get(1).is_some_and(|value| value == "seed-smoke") {
            SEED_SMOKE_SQL
        } else {
            SEED_SQL
        };
        sqlx::raw_sql(sql).execute(&mut connection).await?;
        let manifests: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM perf_integrity.manifests")
            .fetch_one(&mut connection)
            .await?;
        let blobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM perf_integrity.blobs")
            .fetch_one(&mut connection)
            .await?;
        println!("seeded manifests={manifests} blobs={blobs}");
        return Ok(());
    }

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await?;

    match args.get(1).map(String::as_str) {
        Some("seed" | "seed-smoke") => unreachable!(),
        Some("compare") => {
            let scenario = args.get(2).map(String::as_str).unwrap_or("semantics");
            let historical = run(&pool, Mode::Historical, scenario, true).await?;
            let materialized = run(&pool, Mode::MaterializedOwned, scenario, true).await?;
            let streaming = run(&pool, Mode::StreamingSorted, scenario, true).await?;
            let prefetch = run(&pool, Mode::StreamingPrefetch, scenario, true).await?;
            assert_eq!(historical.issues, materialized.issues);
            assert_eq!(historical.issues, streaming.issues);
            assert_eq!(historical.issues, prefetch.issues);
            assert!(materialized.phase_calls <= historical.phase_calls);
            assert_eq!(materialized.phase_calls, streaming.phase_calls);
            assert_eq!(materialized.phase_calls, prefetch.phase_calls);
            assert_eq!(historical.manifest_rows, streaming.manifest_rows);
            assert_eq!(historical.manifest_rows, prefetch.manifest_rows);
            assert_eq!(historical.queries, streaming.queries);
            assert_eq!(historical.queries, prefetch.queries);
            println!(
                "scenario={scenario} issues={} checksum={} phase_calls={}/{}/{}/{} full_calls={}/{}/{}/{} rows={} queries={} streaming_held_connection={} prefetch_held_connection={}",
                historical.issues.len(),
                checksum(&historical.issues),
                historical.phase_calls,
                materialized.phase_calls,
                streaming.phase_calls,
                prefetch.phase_calls,
                historical.full_calls,
                materialized.full_calls,
                streaming.full_calls,
                prefetch.full_calls,
                historical.manifest_rows,
                streaming.queries,
                streaming.held_connection_while_streaming,
                prefetch.held_connection_while_streaming,
            );
        }
        Some("run") => {
            let mode = Mode::parse(args.get(2).map(String::as_str).unwrap_or("streaming"));
            let scenario = args.get(3).map(String::as_str).unwrap_or("large");
            let full = args.get(4).is_some_and(|value| value == "full");
            let outcome = run(&pool, mode, scenario, full).await?;
            println!(
                "mode={mode:?} scenario={scenario} full={full} phase_ms={:.6} full_ms={:.6} issues={} checksum={} phase_calls={} full_calls={} rows={} queries={} streaming_held_connection={}",
                outcome.phase_elapsed.as_secs_f64() * 1e3,
                outcome.full_elapsed.as_secs_f64() * 1e3,
                outcome.issues.len(),
                checksum(&outcome.issues),
                outcome.phase_calls,
                outcome.full_calls,
                outcome.manifest_rows,
                outcome.queries,
                outcome.held_connection_while_streaming,
            );
        }
        _ => panic!(
            "usage: verify_integrity_streaming seed|compare SCENARIO|run MODE SCENARIO [full]"
        ),
    }
    Ok(())
}
