//! Rejected diagnostic harness for the delta loose-chunk write path.
//!
//! It models an idempotent remote object store whose PUT overwrites the same
//! key (the behaviour of the current S3/Azure adapters) and counts physical
//! PUT calls/bytes. The measured prefilter candidate was rolled back because
//! it regressed the normal negotiated-miss path and weakened self-healing.

use bytes::Bytes;
use futures::stream;
use oxicloud::application::ports::blob_storage_ports::{
    BlobStorageBackend, BlobStream, StorageHealthStatus,
};
use oxicloud::domain::errors::DomainError;
use oxicloud::infrastructure::services::dedup_service::DedupService;
use sqlx::postgres::PgPoolOptions;
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use uuid::Uuid;

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Default)]
struct CountingRemote {
    enable_prefilter: bool,
    objects: Mutex<HashMap<String, Bytes>>,
    puts: AtomicU64,
    put_bytes: AtomicU64,
    exists_calls: AtomicU64,
    sync_calls: AtomicU64,
    sync_hashes: AtomicU64,
}

impl CountingRemote {
    fn new(enable_prefilter: bool) -> Self {
        Self {
            enable_prefilter,
            ..Self::default()
        }
    }

    fn reset(&self) {
        self.puts.store(0, Ordering::Relaxed);
        self.put_bytes.store(0, Ordering::Relaxed);
        self.exists_calls.store(0, Ordering::Relaxed);
        self.sync_calls.store(0, Ordering::Relaxed);
        self.sync_hashes.store(0, Ordering::Relaxed);
    }
}

impl BlobStorageBackend for CountingRemote {
    fn initialize(&self) -> BoxFut<'_, Result<(), DomainError>> {
        Box::pin(async { Ok(()) })
    }

    fn put_blob(&self, _hash: &str, _source_path: &Path) -> BoxFut<'_, Result<u64, DomainError>> {
        Box::pin(async {
            Err(DomainError::internal_error(
                "probe",
                "put_blob is outside this probe",
            ))
        })
    }

    fn put_blob_from_bytes(&self, hash: &str, data: Bytes) -> BoxFut<'_, Result<u64, DomainError>> {
        self.puts.fetch_add(1, Ordering::Relaxed);
        self.put_bytes
            .fetch_add(data.len() as u64, Ordering::Relaxed);
        self.objects
            .lock()
            .unwrap()
            .insert(hash.to_string(), data.clone());
        Box::pin(async move { Ok(data.len() as u64) })
    }

    fn put_blob_from_bytes_unsynced(
        &self,
        hash: &str,
        data: Bytes,
    ) -> BoxFut<'_, Result<u64, DomainError>> {
        self.put_blob_from_bytes(hash, data)
    }

    fn sync_blobs(&self, hashes: &[String]) -> BoxFut<'_, Result<(), DomainError>> {
        self.sync_calls.fetch_add(1, Ordering::Relaxed);
        self.sync_hashes
            .fetch_add(hashes.len() as u64, Ordering::Relaxed);
        Box::pin(async { Ok(()) })
    }

    fn get_blob_stream(&self, hash: &str) -> BoxFut<'_, Result<BlobStream, DomainError>> {
        let data = self.objects.lock().unwrap().get(hash).cloned();
        Box::pin(async move {
            let data = data.ok_or_else(|| DomainError::not_found("probe blob", "missing"))?;
            Ok(Box::pin(stream::once(async move { Ok(data) })) as BlobStream)
        })
    }

    fn get_blob_range_stream(
        &self,
        hash: &str,
        _start: u64,
        _end: Option<u64>,
    ) -> BoxFut<'_, Result<BlobStream, DomainError>> {
        self.get_blob_stream(hash)
    }

    fn delete_blob(&self, hash: &str) -> BoxFut<'_, Result<(), DomainError>> {
        self.objects.lock().unwrap().remove(hash);
        Box::pin(async { Ok(()) })
    }

    fn blob_exists(&self, hash: &str) -> BoxFut<'_, Result<bool, DomainError>> {
        self.exists_calls.fetch_add(1, Ordering::Relaxed);
        let present = self.objects.lock().unwrap().contains_key(hash);
        Box::pin(async move { Ok(present) })
    }

    fn blob_size(&self, hash: &str) -> BoxFut<'_, Result<u64, DomainError>> {
        let size = self
            .objects
            .lock()
            .unwrap()
            .get(hash)
            .map_or(0, |data| data.len() as u64);
        Box::pin(async move { Ok(size) })
    }

    fn health_check(&self) -> BoxFut<'_, Result<StorageHealthStatus, DomainError>> {
        Box::pin(async {
            Ok(StorageHealthStatus {
                connected: true,
                backend_type: "counting-remote".into(),
                message: "probe".into(),
                available_bytes: None,
            })
        })
    }

    fn backend_type(&self) -> &'static str {
        if self.enable_prefilter {
            "counting-remote"
        } else {
            // Selects the production raw-local fast path while retaining the
            // same remote-style physical PUT counter for an in-binary A/B.
            "local"
        }
    }

    fn local_blob_path(&self, _hash: &str) -> Option<PathBuf> {
        None
    }
}

fn payloads(seed: u128, count: usize, size: usize) -> Vec<Bytes> {
    (0..count)
        .map(|index| {
            let mut data = vec![0_u8; size];
            data[..16].copy_from_slice(&seed.to_le_bytes());
            data[16..24].copy_from_slice(&(index as u64).to_le_bytes());
            Bytes::from(data)
        })
        .collect()
}

async fn run_case(
    name: &str,
    service: &DedupService,
    backend: &CountingRemote,
    pool: &sqlx::PgPool,
    frames: &[Bytes],
) -> Vec<String> {
    let hashes: Vec<String> = frames
        .iter()
        .map(|frame| blake3::hash(frame).to_hex().to_string())
        .collect();
    let existing_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM storage.blobs WHERE hash = ANY($1::text[])")
            .bind(&hashes)
            .fetch_one(pool)
            .await
            .unwrap();
    backend.reset();
    let input = stream::iter(frames.iter().cloned().map(Ok::<_, DomainError>));
    let started = Instant::now();
    let received = service.store_loose_chunks(input).await.unwrap();
    let elapsed = started.elapsed();
    println!(
        "{name}: frames={} existing_before={} logical_bytes={} heads={} puts={} physical_put_bytes={} sync_calls={} sync_hashes={} elapsed_ms={:.3}",
        frames.len(),
        existing_before,
        frames.iter().map(Bytes::len).sum::<usize>(),
        backend.exists_calls.load(Ordering::Relaxed),
        backend.puts.load(Ordering::Relaxed),
        backend.put_bytes.load(Ordering::Relaxed),
        backend.sync_calls.load(Ordering::Relaxed),
        backend.sync_hashes.load(Ordering::Relaxed),
        elapsed.as_secs_f64() * 1000.0,
    );
    received.into_iter().map(|(hash, _)| hash).collect()
}

#[tokio::main]
async fn main() {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL is required");
    let pool = Arc::new(
        PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap(),
    );
    let enable_prefilter = std::env::var("PROBE_PREFILTER").map_or(true, |v| v != "0");
    println!("prefilter={enable_prefilter}");
    let backend = Arc::new(CountingRemote::new(enable_prefilter));
    let service = DedupService::new(backend.clone(), pool.clone(), pool.clone());

    // 400 × 256 KiB = exactly 100 MiB, the default per-request byte budget.
    let seed = Uuid::new_v4().as_u128();
    let known = payloads(seed, 400, 256 * 1024);
    let fresh = payloads(seed.wrapping_add(1), 400, 256 * 1024);
    let half_fresh = payloads(seed.wrapping_add(2), 200, 256 * 1024);

    let mut cleanup = run_case("seed", &service, &backend, pool.as_ref(), &known).await;
    cleanup.extend(run_case("all_hit", &service, &backend, pool.as_ref(), &known).await);
    cleanup.extend(run_case("all_miss", &service, &backend, pool.as_ref(), &fresh).await);

    let mixed: Vec<Bytes> = known[..200].iter().chain(&half_fresh).cloned().collect();
    cleanup.extend(run_case("half_hit", &service, &backend, pool.as_ref(), &mixed).await);

    cleanup.sort_unstable();
    cleanup.dedup();
    sqlx::query("DELETE FROM storage.blobs WHERE hash = ANY($1::text[])")
        .bind(&cleanup)
        .execute(pool.as_ref())
        .await
        .unwrap();
}
