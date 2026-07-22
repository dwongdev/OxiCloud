//! Rejected diagnostic A/B for identical-content overwrite reference accounting.
//!
//! The caller supplies a disposable PostgreSQL database containing the minimal
//! storage schema used below. This exercises the public FileWritePort method,
//! not a copy of `swap_blob_hash`.

use moka::sync::Cache;
use oxicloud::application::ports::blob_lifecycle::BlobLifecycleHook;
use oxicloud::application::ports::blob_storage_ports::BlobStorageBackend;
use oxicloud::application::ports::storage_ports::FileWritePort;
use oxicloud::application::services::blob_lifecycle_service::BlobLifecycleService;
use oxicloud::infrastructure::repositories::pg::file_blob_write_repository::FileBlobWriteRepository;
use oxicloud::infrastructure::services::dedup_service::DedupService;
use oxicloud::infrastructure::services::local_blob_backend::LocalBlobBackend;
use sqlx::postgres::PgPoolOptions;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use uuid::Uuid;

#[derive(Default)]
struct RecordingBlobHook {
    deleted: Mutex<Vec<String>>,
}

impl RecordingBlobHook {
    fn deleted(&self) -> Vec<String> {
        self.deleted.lock().unwrap().clone()
    }

    fn clear(&self) {
        self.deleted.lock().unwrap().clear();
    }
}

impl BlobLifecycleHook for RecordingBlobHook {
    fn on_blob_created(&self, _blob_hash: &str, _content_type: Option<&str>) {}

    fn on_blob_deleted(&self, blob_hash: &str) {
        self.deleted.lock().unwrap().push(blob_hash.to_string());
    }
}

async fn reset(pool: &sqlx::PgPool) {
    sqlx::query("TRUNCATE storage.files, storage.chunk_manifests, storage.blobs")
        .execute(pool)
        .await
        .unwrap();
}

async fn ref_count(pool: &sqlx::PgPool, hash: &str) -> Option<i32> {
    sqlx::query_scalar("SELECT ref_count FROM storage.blobs WHERE hash = $1")
        .bind(hash)
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn manifest_ref_count(pool: &sqlx::PgPool, hash: &str) -> Option<i32> {
    sqlx::query_scalar("SELECT ref_count FROM storage.chunk_manifests WHERE file_hash = $1")
        .bind(hash)
        .fetch_optional(pool)
        .await
        .unwrap()
}

fn percentile_ms(samples_ns: &mut [u128], percentile: usize) -> f64 {
    samples_ns.sort_unstable();
    let index = (samples_ns.len() - 1) * percentile / 100;
    samples_ns[index] as f64 / 1_000_000.0
}

#[tokio::main]
async fn main() {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL is required");
    let iterations = std::env::var("ITERATIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1_000);
    let different_iterations = std::env::var("DIFFERENT_ITERATIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(100);
    let pool = Arc::new(
        PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await
            .unwrap(),
    );
    let temp = tempfile::tempdir().unwrap();
    let backend = Arc::new(LocalBlobBackend::new(Path::new(temp.path())));
    backend.initialize().await.unwrap();
    let recording_hook = Arc::new(RecordingBlobHook::default());
    let lifecycle = Arc::new(
        BlobLifecycleService::new().with_hook(recording_hook.clone() as Arc<dyn BlobLifecycleHook>),
    );
    let dedup = Arc::new(
        DedupService::new(backend, pool.clone(), pool.clone()).with_blob_lifecycle(lifecycle),
    );
    let repo = FileBlobWriteRepository::new(
        pool.clone(),
        dedup.clone(),
        Cache::builder().max_capacity(16).build(),
    );
    let caller = Uuid::new_v4();
    let file_id = Uuid::new_v4();
    let hash_a = blake3::hash(b"same-content").to_hex().to_string();

    reset(pool.as_ref()).await;
    sqlx::query("INSERT INTO storage.blobs (hash, size, ref_count) VALUES ($1, 12, 1)")
        .bind(&hash_a)
        .execute(pool.as_ref())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO storage.files (id, blob_hash, size, updated_by) VALUES ($1, $2, 12, $3)",
    )
    .bind(file_id)
    .bind(&hash_a)
    .bind(caller)
    .execute(pool.as_ref())
    .await
    .unwrap();

    let started = Instant::now();
    let mut identical_samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let iteration_started = Instant::now();
        // Models the reference acquired by the ingest layer immediately before
        // FileWritePort consumes it.
        dedup.add_reference(&hash_a).await.unwrap();
        repo.update_file_content_with_blob(&file_id.to_string(), &hash_a, 12, None, caller)
            .await
            .unwrap();
        identical_samples.push(iteration_started.elapsed().as_nanos());
    }
    let identical_ref = ref_count(pool.as_ref(), &hash_a).await;
    let identical_p50 = percentile_ms(&mut identical_samples, 50);
    let identical_p95 = percentile_ms(&mut identical_samples, 95);
    println!(
        "identical: iterations={iterations} final_ref_count={:?} elapsed_ms={:.3} \
         p50_ms={identical_p50:.3} p95_ms={identical_p95:.3} roundtrips_per_iteration=3",
        identical_ref,
        started.elapsed().as_secs_f64() * 1_000.0
    );
    assert_eq!(identical_ref, Some(1));
    assert!(recording_hook.deleted().is_empty());

    // Same-hash CDC manifest control. A single-chunk file deliberately has a
    // row in both tables under the same hash: only the manifest reference is
    // file-level and the chunk row must remain unchanged.
    reset(pool.as_ref()).await;
    recording_hook.clear();
    let manifest_iterations = iterations.min(100);
    sqlx::query("INSERT INTO storage.blobs (hash, size, ref_count) VALUES ($1, 12, 1)")
        .bind(&hash_a)
        .execute(pool.as_ref())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO storage.chunk_manifests
             (file_hash, chunk_hashes, chunk_sizes, total_size, chunk_count, ref_count)
         VALUES ($1, ARRAY[$1], ARRAY[12::bigint], 12, 1, 1)",
    )
    .bind(&hash_a)
    .execute(pool.as_ref())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO storage.files (id, blob_hash, size, updated_by) VALUES ($1, $2, 12, $3)",
    )
    .bind(file_id)
    .bind(&hash_a)
    .bind(caller)
    .execute(pool.as_ref())
    .await
    .unwrap();
    let started = Instant::now();
    let mut manifest_samples = Vec::with_capacity(manifest_iterations);
    for _ in 0..manifest_iterations {
        let iteration_started = Instant::now();
        dedup.add_reference(&hash_a).await.unwrap();
        repo.update_file_content_with_blob(&file_id.to_string(), &hash_a, 12, None, caller)
            .await
            .unwrap();
        manifest_samples.push(iteration_started.elapsed().as_nanos());
    }
    let manifest_ref = manifest_ref_count(pool.as_ref(), &hash_a).await;
    let chunk_ref = ref_count(pool.as_ref(), &hash_a).await;
    let manifest_p50 = percentile_ms(&mut manifest_samples, 50);
    let manifest_p95 = percentile_ms(&mut manifest_samples, 95);
    println!(
        "identical_manifest: iterations={manifest_iterations} manifest_ref={manifest_ref:?} \
         chunk_ref={chunk_ref:?} elapsed_ms={:.3} p50_ms={manifest_p50:.3} \
         p95_ms={manifest_p95:.3} roundtrips_per_iteration=2",
        started.elapsed().as_secs_f64() * 1_000.0
    );
    assert_eq!(manifest_ref, Some(1));
    assert_eq!(chunk_ref, Some(1));
    assert!(recording_hook.deleted().is_empty());

    // Alternating-content latency control. A permanent base reference keeps
    // both blobs alive, isolating the normal different-hash decrement path.
    reset(pool.as_ref()).await;
    recording_hook.clear();
    let hash_b = blake3::hash(b"different-content").to_hex().to_string();
    sqlx::query(
        "INSERT INTO storage.blobs (hash, size, ref_count)
         VALUES ($1, 12, 2), ($2, 17, 1)",
    )
    .bind(&hash_a)
    .bind(&hash_b)
    .execute(pool.as_ref())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO storage.files (id, blob_hash, size, updated_by) VALUES ($1, $2, 12, $3)",
    )
    .bind(file_id)
    .bind(&hash_a)
    .bind(caller)
    .execute(pool.as_ref())
    .await
    .unwrap();
    let started = Instant::now();
    let mut alternating_samples = Vec::with_capacity(different_iterations);
    for i in 0..different_iterations {
        let iteration_started = Instant::now();
        let (target, size) = if i % 2 == 0 {
            (&hash_b, 17)
        } else {
            (&hash_a, 12)
        };
        dedup.add_reference(target).await.unwrap();
        repo.update_file_content_with_blob(&file_id.to_string(), target, size, None, caller)
            .await
            .unwrap();
        alternating_samples.push(iteration_started.elapsed().as_nanos());
    }
    let alternating_a_ref = ref_count(pool.as_ref(), &hash_a).await;
    let alternating_b_ref = ref_count(pool.as_ref(), &hash_b).await;
    let expected_a = if different_iterations % 2 == 0 { 2 } else { 1 };
    let expected_b = if different_iterations % 2 == 0 { 1 } else { 2 };
    let alternating_p50 = percentile_ms(&mut alternating_samples, 50);
    let alternating_p95 = percentile_ms(&mut alternating_samples, 95);
    println!(
        "alternating: iterations={different_iterations} a_ref={alternating_a_ref:?} \
         b_ref={alternating_b_ref:?} elapsed_ms={:.3} p50_ms={alternating_p50:.3} \
         p95_ms={alternating_p95:.3} roundtrips_per_iteration=8",
        started.elapsed().as_secs_f64() * 1_000.0
    );
    assert_eq!(alternating_a_ref, Some(expected_a));
    assert_eq!(alternating_b_ref, Some(expected_b));
    assert!(recording_hook.deleted().is_empty());

    // Different-content control: the old reference must disappear, the new
    // reference must remain exactly once.
    reset(pool.as_ref()).await;
    recording_hook.clear();
    sqlx::query(
        "INSERT INTO storage.blobs (hash, size, ref_count)
         VALUES ($1, 12, 1), ($2, 17, 1)",
    )
    .bind(&hash_a)
    .bind(&hash_b)
    .execute(pool.as_ref())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO storage.files (id, blob_hash, size, updated_by) VALUES ($1, $2, 12, $3)",
    )
    .bind(file_id)
    .bind(&hash_a)
    .bind(caller)
    .execute(pool.as_ref())
    .await
    .unwrap();
    repo.update_file_content_with_blob(&file_id.to_string(), &hash_b, 17, None, caller)
        .await
        .unwrap();
    let old_ref = ref_count(pool.as_ref(), &hash_a).await;
    let new_ref = ref_count(pool.as_ref(), &hash_b).await;
    println!("different: old_ref={:?} new_ref={:?}", old_ref, new_ref);
    assert_eq!(old_ref, None);
    assert_eq!(new_ref, Some(1));
    assert_eq!(recording_hook.deleted(), vec![hash_a.clone()]);

    repo.delete_file(&file_id.to_string()).await.unwrap();
    let (deleted, _) = dedup.garbage_collect_force().await.unwrap();
    let final_new_ref = ref_count(pool.as_ref(), &hash_b).await;
    println!(
        "delete_gc: deleted={deleted} final_new_ref={:?}",
        final_new_ref
    );
    assert_eq!(deleted, 1);
    assert_eq!(final_new_ref, None);
    assert_eq!(
        recording_hook.deleted(),
        vec![hash_a.clone(), hash_b.clone()]
    );

    // Missing-file compensation consumes the incoming reference and fires the
    // deletion hook when it was the only one.
    reset(pool.as_ref()).await;
    recording_hook.clear();
    let missing_hash = blake3::hash(b"missing-target").to_hex().to_string();
    sqlx::query("INSERT INTO storage.blobs (hash, size, ref_count) VALUES ($1, 14, 1)")
        .bind(&missing_hash)
        .execute(pool.as_ref())
        .await
        .unwrap();
    assert!(
        repo.update_file_content_with_blob(
            &Uuid::new_v4().to_string(),
            &missing_hash,
            14,
            None,
            caller,
        )
        .await
        .is_err()
    );
    assert_eq!(ref_count(pool.as_ref(), &missing_hash).await, None);
    assert_eq!(recording_hook.deleted(), vec![missing_hash.clone()]);
    println!("missing_compensation: ref=None hook=1");

    // SQL-error compensation (invalid UUID cast) follows the distinct Err
    // branch and must likewise consume the incoming reference exactly once.
    reset(pool.as_ref()).await;
    recording_hook.clear();
    let error_hash = blake3::hash(b"sql-error").to_hex().to_string();
    sqlx::query("INSERT INTO storage.blobs (hash, size, ref_count) VALUES ($1, 9, 1)")
        .bind(&error_hash)
        .execute(pool.as_ref())
        .await
        .unwrap();
    assert!(
        repo.update_file_content_with_blob("not-a-uuid", &error_hash, 9, None, caller)
            .await
            .is_err()
    );
    assert_eq!(ref_count(pool.as_ref(), &error_hash).await, None);
    assert_eq!(recording_hook.deleted(), vec![error_hash]);
    println!("error_compensation: ref=None hook=1");
    reset(pool.as_ref()).await;
}
