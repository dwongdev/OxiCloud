//! Real-PostgreSQL gate for the blob-migration work set.
//!
//! `current` reproduces `run_migration` collecting every `(hash, size)` row
//! before starting backend work. `paged` uses indexed keyset pages and drops
//! each page after consuming it. The checksum/count gate proves that both
//! consume the exact same ordered rows.

use futures::TryStreamExt;
use sqlx::postgres::PgPoolOptions;
use std::hint::black_box;
use std::time::{Duration, Instant};

async fn connect_with_retry(url: &str) -> sqlx::PgPool {
    let mut last_error = None;
    for _ in 0..12 {
        match PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(5))
            .connect(url)
            .await
        {
            Ok(pool) => return pool,
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    panic!(
        "connect PostgreSQL after retries: {}",
        last_error.expect("at least one connection attempt")
    );
}

async fn seed(pool: &sqlx::PgPool, rows: i64) {
    sqlx::query("CREATE SCHEMA IF NOT EXISTS storage")
        .execute(pool)
        .await
        .expect("create storage schema");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS storage.blobs (
             hash text PRIMARY KEY,
             size bigint NOT NULL
         )",
    )
    .execute(pool)
    .await
    .expect("create blob table");
    sqlx::query("TRUNCATE storage.blobs")
        .execute(pool)
        .await
        .expect("truncate blob table");
    sqlx::query(
        "INSERT INTO storage.blobs(hash, size)
         SELECT lpad(to_hex(n), 64, '0'), 1024 + (n % 1048576)
           FROM generate_series(1, $1) AS n",
    )
    .bind(rows)
    .execute(pool)
    .await
    .expect("seed blob table");
    sqlx::query("ANALYZE storage.blobs")
        .execute(pool)
        .await
        .expect("analyze blob table");
}

fn consume(checksum: &mut u64, hash: &str, size: i64) {
    let first = hash.as_bytes().first().copied().unwrap_or_default() as u64;
    let last = hash.as_bytes().last().copied().unwrap_or_default() as u64;
    *checksum = checksum
        .wrapping_mul(0x100_0000_01b3)
        .wrapping_add(first)
        .wrapping_add(last << 8)
        .wrapping_add(size as u64);
    black_box(checksum);
}

async fn current(pool: &sqlx::PgPool) -> (usize, u64, usize) {
    let work: Vec<(String, i64)> =
        sqlx::query_as("SELECT hash, size FROM storage.blobs ORDER BY hash")
            .fetch(pool)
            .try_collect()
            .await
            .expect("fetch current work set");
    let peak_rows = work.len();
    let mut checksum = 0_u64;
    for (hash, size) in &work {
        consume(&mut checksum, hash, *size);
    }
    (work.len(), checksum, peak_rows)
}

async fn streamed(pool: &sqlx::PgPool) -> (usize, u64, usize) {
    let mut rows =
        sqlx::query_as::<_, (String, i64)>("SELECT hash, size FROM storage.blobs ORDER BY hash")
            .fetch(pool);
    let mut count = 0usize;
    let mut checksum = 0_u64;
    while let Some(row) = rows.try_next().await.expect("stream work row") {
        consume(&mut checksum, &row.0, row.1);
        count += 1;
    }
    (count, checksum, 1)
}

async fn paged(pool: &sqlx::PgPool, page_size: i64) -> (usize, u64, usize) {
    let mut after = String::new();
    let mut count = 0usize;
    let mut checksum = 0_u64;
    let mut peak_rows = 0usize;
    loop {
        let page: Vec<(String, i64)> = sqlx::query_as(
            "SELECT hash, size FROM storage.blobs
              WHERE hash > $1 ORDER BY hash LIMIT $2",
        )
        .bind(&after)
        .bind(page_size)
        .fetch_all(pool)
        .await
        .expect("fetch keyset page");
        if page.is_empty() {
            break;
        }
        peak_rows = peak_rows.max(page.len());
        after.clone_from(&page.last().expect("non-empty page").0);
        for (hash, size) in &page {
            consume(&mut checksum, hash, *size);
        }
        count += page.len();
    }
    (count, checksum, peak_rows)
}

#[tokio::main]
async fn main() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is required");
    let mode = std::env::args().nth(1).unwrap_or_else(|| "current".into());
    let value = std::env::args()
        .nth(2)
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(1_000_000);
    // The local Docker bridge occasionally drops a new host-side connection.
    // Connection retries happen before the timed region and apply identically
    // to every mode, so transport setup cannot skew an algorithm sample.
    let pool = connect_with_retry(&url).await;

    if mode == "seed" {
        seed(&pool, value).await;
        println!("seeded_rows={value}");
        return;
    }

    // Warm the PostgreSQL/index pages without retaining Rust rows.
    let _: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM storage.blobs")
        .fetch_one(&pool)
        .await
        .expect("warm count");
    let started = Instant::now();
    let (rows, checksum, peak_rows) = match mode.as_str() {
        "current" => current(&pool).await,
        "stream" => streamed(&pool).await,
        "paged" => paged(&pool, value).await,
        other => panic!("unknown mode: {other}"),
    };
    println!(
        "mode={mode} value={value} rows={rows} checksum={checksum} peak_rows={peak_rows} elapsed_ms={:.3}",
        started.elapsed().as_secs_f64() * 1_000.0
    );
}
