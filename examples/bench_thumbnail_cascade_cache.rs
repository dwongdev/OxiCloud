//! Shared-album thumbnail authz benchmark — folder-grant cascade query per
//! thumbnail vs the `cascade_grant_cache`.
//!
//! A recipient of a shared folder (a grant on the album folder, NOT drive
//! membership) fails the drive-role precheck in `PgAclEngine::check_inner` and
//! falls through to `file_cascade_grant_exists` — an ltree folder-ancestor
//! grant query — for EVERY file. `get_thumbnail_impl` runs that Read check on
//! every request, and browsers revalidate immutable thumbnails constantly
//! (`If-None-Match`), so the same `(recipient, file, Read)` decision is
//! recomputed again and again: ~one grant query per thumbnail per view.
//!
//! Round 8 memoises that decision in `cascade_grant_cache` (30 s TTL, flushed
//! on any File/Folder grant write). The check still runs on every request —
//! it is never skipped — but after the first query it resolves in-memory.
//!
//! Safety gates (hard asserts, exit 1 on failure):
//!   1. the folder-grant recipient is allowed; an outsider is denied;
//!   2. REVOCATION — after a warm cache serves `allowed`, `clear_role` on the
//!      shared folder makes the very next check DENY (proves the grant-write
//!      invalidation flushes the cache; without it the stale `true` would
//!      still serve).
//!
//! Run (needs Postgres up; reads DATABASE_URL from .env):
//!   cargo run --release --features bench --example bench_thumbnail_cascade_cache
//! Tunables (env): BENCH_THUMBS (100), BENCH_POOL (8).

use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use oxicloud::application::ports::authorization_ports::AuthorizationEngine;
use oxicloud::domain::services::authorization::{Permission, Resource, Role, Subject};
use oxicloud::infrastructure::repositories::pg::{
    FileBlobReadRepository, FolderDbRepository, SubjectGroupPgRepository,
};
use oxicloud::infrastructure::services::dedup_service::DedupService;
use oxicloud::infrastructure::services::local_blob_backend::LocalBlobBackend;
use oxicloud::infrastructure::services::pg_acl_engine::PgAclEngine;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct Seeded {
    owner: Uuid,
    recipient: Uuid,
    outsider: Uuid,
    drive_id: Uuid,
    root_folder: Uuid,
    album_folder: Uuid,
    blob_hash: String,
    files: Vec<Uuid>,
}

async fn seed(pool: &PgPool, n_thumbs: usize) -> Seeded {
    let mut tx = pool.begin().await.expect("begin");
    let owner: Uuid = sqlx::query_scalar(
        "INSERT INTO auth.users (username, email, role)
         VALUES ('bench_thumbowner', 'bench_thumbowner@bench.invalid', 'user') RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("seed owner");
    let recipient: Uuid = sqlx::query_scalar(
        "INSERT INTO auth.users (username, email, role)
         VALUES ('bench_thumbrecip', 'bench_thumbrecip@bench.invalid', 'user') RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("seed recipient");
    let outsider: Uuid = sqlx::query_scalar(
        "INSERT INTO auth.users (username, email, role)
         VALUES ('bench_thumbout', 'bench_thumbout@bench.invalid', 'user') RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("seed outsider");

    // Owner's personal drive with a root and an album subfolder. The recipient
    // is NOT a drive member — only granted the album folder below, so their
    // File checks fall through the drive precheck to the folder cascade.
    let drive_id: Uuid = sqlx::query_scalar(
        "INSERT INTO storage.drives (kind, default_for_user) VALUES ('personal', $1) RETURNING id",
    )
    .bind(owner)
    .fetch_one(&mut *tx)
    .await
    .expect("seed drive");
    let root_folder: Uuid = sqlx::query_scalar(
        "INSERT INTO storage.folders (name, path, lpath, drive_id)
         VALUES ('Personal', '/Personal', 'benchthumbroot', $1) RETURNING id",
    )
    .bind(drive_id)
    .fetch_one(&mut *tx)
    .await
    .expect("seed root");
    sqlx::query("UPDATE storage.drives SET root_folder_id = $1 WHERE id = $2")
        .bind(root_folder)
        .bind(drive_id)
        .execute(&mut *tx)
        .await
        .expect("stamp root");
    let album_folder: Uuid = sqlx::query_scalar(
        "INSERT INTO storage.folders (name, path, lpath, drive_id, parent_id)
         VALUES ('Album', '/Personal/Album', 'benchthumbroot.album', $1, $2) RETURNING id",
    )
    .bind(drive_id)
    .bind(root_folder)
    .fetch_one(&mut *tx)
    .await
    .expect("seed album");
    // Owner grant on the drive (personal-drive owner floor), and the recipient
    // grant on the ALBUM FOLDER only — the shared-album shape.
    sqlx::query(
        "INSERT INTO storage.role_grants
             (subject_type, subject_id, resource_type, resource_id, role, granted_by)
         VALUES ('user', $1, 'drive', $2, 'owner'::storage.grant_role, $1)",
    )
    .bind(owner)
    .bind(drive_id)
    .execute(&mut *tx)
    .await
    .expect("seed owner grant");
    sqlx::query(
        "INSERT INTO storage.role_grants
             (subject_type, subject_id, resource_type, resource_id, role, granted_by)
         VALUES ('user', $1, 'folder', $2, 'viewer'::storage.grant_role, $3)",
    )
    .bind(recipient)
    .bind(album_folder)
    .bind(owner)
    .execute(&mut *tx)
    .await
    .expect("seed recipient folder grant");

    let blob_hash = "benchthumbcascade00000000000000000000000000000000000000000000b4".to_string();
    sqlx::query("INSERT INTO storage.blobs (hash, size, ref_count) VALUES ($1, 4096, 1)")
        .bind(&blob_hash)
        .execute(&mut *tx)
        .await
        .expect("seed blob");
    let mut files = Vec::with_capacity(n_thumbs);
    for i in 0..n_thumbs {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO storage.files (name, folder_id, blob_hash, size, mime_type, drive_id)
             VALUES ($1, $2, $3, 4096, 'image/jpeg', $4) RETURNING id",
        )
        .bind(format!("photo-{i:04}.jpg"))
        .bind(album_folder)
        .bind(&blob_hash)
        .bind(drive_id)
        .fetch_one(&mut *tx)
        .await
        .expect("seed file");
        files.push(id);
    }
    tx.commit().await.expect("commit");
    Seeded {
        owner,
        recipient,
        outsider,
        drive_id,
        root_folder,
        album_folder,
        blob_hash,
        files,
    }
}

async fn cleanup(pool: &PgPool, s: &Seeded) {
    let _ = sqlx::query(
        "DELETE FROM storage.role_grants WHERE resource_id IN ($1, $2) OR resource_id = ANY($3)",
    )
    .bind(s.drive_id)
    .bind(s.album_folder)
    .bind(&s.files)
    .execute(pool)
    .await;
    let _ = sqlx::query("DELETE FROM storage.files WHERE drive_id = $1")
        .bind(s.drive_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM storage.drives WHERE id = $1")
        .bind(s.drive_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM storage.folders WHERE id IN ($1, $2)")
        .bind(s.album_folder)
        .bind(s.root_folder)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM storage.blobs WHERE hash = $1")
        .bind(&s.blob_hash)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM auth.users WHERE id IN ($1, $2, $3)")
        .bind(s.owner)
        .bind(s.recipient)
        .bind(s.outsider)
        .execute(pool)
        .await;
}

fn fresh_engine(pool: &Arc<PgPool>) -> Arc<PgAclEngine> {
    let folder_repo = Arc::new(FolderDbRepository::new(pool.clone()));
    let backend = Arc::new(LocalBlobBackend::new(std::path::Path::new(
        "/tmp/bench-thumbcascade-blobs",
    )));
    let dedup = Arc::new(DedupService::new(backend, pool.clone(), pool.clone()));
    let file_repo = Arc::new(FileBlobReadRepository::new(
        pool.clone(),
        dedup,
        folder_repo.clone(),
    ));
    let group_repo = Arc::new(SubjectGroupPgRepository::new(pool.clone()));
    Arc::new(PgAclEngine::new(
        pool.clone(),
        folder_repo,
        file_repo,
        group_repo,
    ))
}

async fn allowed(engine: &Arc<PgAclEngine>, caller: Uuid, file: Uuid) -> bool {
    engine
        .require(
            Subject::User(caller),
            Permission::Read,
            Resource::File(file),
        )
        .await
        .is_ok()
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    dotenvy::dotenv().ok();
    let url = env::var("DATABASE_URL")
        .or_else(|_| env::var("OXICLOUD_DB_CONNECTION_STRING"))
        .expect("set DATABASE_URL — the dev Postgres URL");
    let thumbs: usize = env_or("BENCH_THUMBS", 100);
    let pool_size: u32 = env_or("BENCH_POOL", 8);

    let pool = Arc::new(
        PgPoolOptions::new()
            .max_connections(pool_size)
            .min_connections(pool_size)
            .acquire_timeout(Duration::from_secs(10))
            .connect(&url)
            .await
            .expect("connect Postgres"),
    );

    let s = seed(&pool, thumbs).await;

    // ── Safety gate 1: recipient allowed on every file, outsider denied ──
    {
        let engine = fresh_engine(&pool);
        for &f in &s.files {
            if !allowed(&engine, s.recipient, f).await {
                eprintln!("SAFETY GATE FAILED: folder-grant recipient denied a file in the album");
                cleanup(&pool, &s).await;
                std::process::exit(1);
            }
        }
        if allowed(&engine, s.outsider, s.files[0]).await {
            eprintln!("SAFETY GATE FAILED: outsider was allowed");
            cleanup(&pool, &s).await;
            std::process::exit(1);
        }
    }

    // ── Safety gate 2: revocation flushes the cache (immediate deny) ──
    {
        let engine = fresh_engine(&pool);
        // Warm: caches (recipient, File[0], Read) → true.
        assert!(allowed(&engine, s.recipient, s.files[0]).await);
        // Revoke the album share through the real grant-write path.
        engine
            .clear_role(Subject::User(s.recipient), Resource::Folder(s.album_folder))
            .await
            .expect("clear_role");
        // Next check MUST deny — a stale cached `true` here would be a hole.
        if allowed(&engine, s.recipient, s.files[0]).await {
            eprintln!(
                "SAFETY GATE FAILED: recipient still allowed after clear_role — \
                 cascade cache was not invalidated on grant revoke"
            );
            cleanup(&pool, &s).await;
            std::process::exit(1);
        }
        // Re-grant for the perf run below.
        engine
            .set_role(
                s.owner,
                Subject::User(s.recipient),
                Role::Viewer,
                Resource::Folder(s.album_folder),
                None,
            )
            .await
            .expect("re-grant");
    }

    println!("\n#################################################################");
    println!("# shared-album thumbnail authz: folder-cascade query/thumb vs cache");
    println!("# thumbs={thumbs}  (recipient holds a folder grant, no drive membership)");
    println!("#################################################################\n");
    println!("| {:<28} | {:>10} | {:>12} |", "arm", "wall ms", "µs/thumb");

    // BEFORE: no cache — a fresh engine per thumbnail forces the cascade query
    // every time (models the pre-round-8 per-request behaviour).
    {
        let t = Instant::now();
        for &f in &s.files {
            let engine = fresh_engine(&pool);
            std::hint::black_box(allowed(&engine, s.recipient, f).await);
        }
        let el = t.elapsed();
        println!(
            "| {:<28} | {:>10.2} | {:>12.2} |",
            "BEFORE (query/thumb)",
            el.as_secs_f64() * 1e3,
            el.as_secs_f64() * 1e6 / thumbs as f64
        );
    }

    // AFTER cold: one persistent engine — the first grid view queries once per
    // distinct file (cache misses populate).
    let engine = fresh_engine(&pool);
    {
        let t = Instant::now();
        for &f in &s.files {
            std::hint::black_box(allowed(&engine, s.recipient, f).await);
        }
        let el = t.elapsed();
        println!(
            "| {:<28} | {:>10.2} | {:>12.2} |",
            "AFTER cold (first view)",
            el.as_secs_f64() * 1e3,
            el.as_secs_f64() * 1e6 / thumbs as f64
        );
    }

    // AFTER warm: revalidation re-checks the same files — all cache hits, the
    // "navigate away and back" / constant If-None-Match revalidation case.
    {
        let t = Instant::now();
        for &f in &s.files {
            std::hint::black_box(allowed(&engine, s.recipient, f).await);
        }
        let el = t.elapsed();
        println!(
            "| {:<28} | {:>10.2} | {:>12.2} |",
            "AFTER warm (revalidation)",
            el.as_secs_f64() * 1e3,
            el.as_secs_f64() * 1e6 / thumbs as f64
        );
    }

    cleanup(&pool, &s).await;
    println!("\n(The check is never skipped — authz still runs on every thumbnail; only");
    println!(" the folder-cascade DECISION is memoised. BEFORE re-queries per request;");
    println!(" AFTER warm serves revalidations from memory. Safety gates verified:");
    println!(" recipient allowed, outsider denied, and a clear_role revoke denies");
    println!(" immediately — the grant write flushed the cache.)");
}
