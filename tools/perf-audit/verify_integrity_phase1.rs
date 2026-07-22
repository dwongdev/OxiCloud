//! Independent A/B for `DedupService::verify_integrity` phase 1.
//!
//! This deliberately does not import the OxiCloud crate. It models the exact
//! manifest validation/messages and a backend whose `blob_size` operation has
//! either local-filesystem scheduling latency or remote request latency.

use foldhash::quality::RandomState;
use futures::stream::{self, StreamExt};
use std::collections::HashMap;
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const CONCURRENCY: usize = 16;
const SERIAL_FAST_PATH_OCCURRENCES: usize = 4;
type Manifest = (String, Vec<String>, Vec<i64>, i64);
type SizeMap = HashMap<String, Option<u64>, RandomState>;

#[inline]
fn uses_serial_fast_path(manifests: &[Manifest]) -> bool {
    if manifests.len() == 1 {
        let (_, hashes, sizes, _) = &manifests[0];
        return hashes.len() != sizes.len() || hashes.len() <= SERIAL_FAST_PATH_OCCURRENCES;
    }
    let mut occurrences = 0usize;
    for (_, hashes, sizes, _) in manifests {
        if hashes.len() == sizes.len() {
            occurrences = occurrences.saturating_add(hashes.len());
            if occurrences > SERIAL_FAST_PATH_OCCURRENCES {
                return false;
            }
        }
    }
    true
}

#[derive(Clone, Copy)]
enum Latency {
    /// No delay: used only by the separate-process peak-RSS probe.
    Immediate,
    /// Warm/cached local metadata latency as observed by an async caller.
    Local { metadata: Duration, hash: Duration },
    /// Object-store HEAD request: asynchronously wait for network latency.
    Remote(Duration),
}

#[derive(Clone)]
struct SimBackend {
    latency: Latency,
    calls: Arc<AtomicUsize>,
}

impl SimBackend {
    fn new(latency: Latency) -> Self {
        Self {
            latency,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    async fn blob_size(&self, hash: &str) -> Option<u64> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        match self.latency {
            Latency::Immediate => {}
            Latency::Local { metadata, .. } => tokio::time::sleep(metadata).await,
            Latency::Remote(delay) => tokio::time::sleep(delay).await,
        }
        if hash.starts_with("missing-") {
            None
        } else if hash.starts_with("wrong-") {
            Some(999)
        } else {
            Some(256)
        }
    }

    async fn hash_local_blob(&self) {
        if let Latency::Local { hash, .. } = self.latency {
            // Equal phase-2 work: model mmap/BLAKE3 verification separately
            // from metadata. The exact value only dilutes the phase-1 win; it
            // does not differ between current and candidate.
            tokio::time::sleep(hash).await;
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

fn label(value: &str) -> &str {
    &value[..value.len().min(12)]
}

async fn current(manifests: &[Manifest], backend: &SimBackend) -> Vec<String> {
    let mut issues = Vec::new();
    for (file_hash, chunk_hashes, chunk_sizes, total_size) in manifests {
        let file_label = label(file_hash);
        if chunk_hashes.len() != chunk_sizes.len() {
            issues.push(format!(
                "Manifest {file_label}: chunk_hashes/chunk_sizes length mismatch"
            ));
            continue;
        }
        let sum: i64 = chunk_sizes.iter().sum();
        if sum != *total_size {
            issues.push(format!(
                "Manifest {file_label}: total_size {total_size} != sum of chunk_sizes {sum}"
            ));
        }
        for (index, chunk_hash) in chunk_hashes.iter().enumerate() {
            let chunk_label = label(chunk_hash);
            match backend.blob_size(chunk_hash).await {
                Some(actual_size) if actual_size != chunk_sizes[index] as u64 => {
                    issues.push(format!(
                        "Manifest {file_label} chunk {chunk_label}: size mismatch \
                         (expected {}, actual {actual_size})",
                        chunk_sizes[index]
                    ));
                }
                None => issues.push(format!(
                    "Manifest {file_label} chunk {chunk_label}: missing in backend"
                )),
                Some(_) => {}
            }
        }
    }
    issues
}

fn replay(manifests: &[Manifest], sizes: &SizeMap) -> Vec<String> {
    let mut issues = Vec::new();
    for (file_hash, chunk_hashes, chunk_sizes, total_size) in manifests {
        let file_label = label(file_hash);
        if chunk_hashes.len() != chunk_sizes.len() {
            issues.push(format!(
                "Manifest {file_label}: chunk_hashes/chunk_sizes length mismatch"
            ));
            continue;
        }
        let sum: i64 = chunk_sizes.iter().sum();
        if sum != *total_size {
            issues.push(format!(
                "Manifest {file_label}: total_size {total_size} != sum of chunk_sizes {sum}"
            ));
        }
        for (index, chunk_hash) in chunk_hashes.iter().enumerate() {
            let chunk_label = label(chunk_hash);
            match sizes.get(chunk_hash.as_str()).copied().flatten() {
                Some(actual_size) if actual_size != chunk_sizes[index] as u64 => {
                    issues.push(format!(
                        "Manifest {file_label} chunk {chunk_label}: size mismatch \
                         (expected {}, actual {actual_size})",
                        chunk_sizes[index]
                    ));
                }
                None => issues.push(format!(
                    "Manifest {file_label} chunk {chunk_label}: missing in backend"
                )),
                Some(_) => {}
            }
        }
    }
    issues
}

const CANDIDATE_BATCH_OCCURRENCES: usize = 256;

async fn fill_sizes(size_by_hash: SizeMap, backend: &SimBackend) -> SizeMap {
    let hashes: Vec<String> = size_by_hash.keys().cloned().collect();
    stream::iter(hashes)
        .map(|hash| async move {
            let size = backend.blob_size(&hash).await;
            (hash, size)
        })
        .buffer_unordered(CONCURRENCY)
        .fold(size_by_hash, |mut sizes, (hash, size)| async move {
            sizes.insert(hash, size);
            sizes
        })
        .await
}

async fn candidate_batch(manifests: &[Manifest], backend: &SimBackend) -> Vec<String> {
    // Invalid manifests are skipped by the current implementation, so their
    // hashes must not become backend calls in the candidate either.
    let mut size_by_hash = SizeMap::default();
    for (_, hashes, chunk_sizes, _) in manifests {
        if hashes.len() == chunk_sizes.len() {
            for hash in hashes {
                size_by_hash.entry(hash.clone()).or_insert(None);
            }
        }
    }
    let size_by_hash = fill_sizes(size_by_hash, backend).await;
    replay(manifests, &size_by_hash)
}

async fn candidate_large_manifest(manifest: &Manifest, backend: &SimBackend) -> Vec<String> {
    let (file_hash, hashes, chunk_sizes, total_size) = manifest;
    let file_label = label(file_hash);
    if hashes.len() != chunk_sizes.len() {
        return vec![format!(
            "Manifest {file_label}: chunk_hashes/chunk_sizes length mismatch"
        )];
    }
    let mut issues = Vec::new();
    let sum: i64 = chunk_sizes.iter().sum();
    if sum != *total_size {
        issues.push(format!(
            "Manifest {file_label}: total_size {total_size} != sum of chunk_sizes {sum}"
        ));
    }
    for offset in (0..hashes.len()).step_by(CANDIDATE_BATCH_OCCURRENCES) {
        let end = (offset + CANDIDATE_BATCH_OCCURRENCES).min(hashes.len());
        let mut size_by_hash = SizeMap::default();
        for hash in &hashes[offset..end] {
            size_by_hash.entry(hash.clone()).or_insert(None);
        }
        let size_by_hash = fill_sizes(size_by_hash, backend).await;
        for (index, hash) in hashes[offset..end].iter().enumerate() {
            let expected = chunk_sizes[offset + index];
            let chunk_label = label(hash);
            match size_by_hash.get(hash.as_str()).copied().flatten() {
                Some(actual) if actual != expected as u64 => issues.push(format!(
                    "Manifest {file_label} chunk {chunk_label}: size mismatch \
                     (expected {expected}, actual {actual})"
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

async fn candidate(manifests: &[Manifest], backend: &SimBackend) -> Vec<String> {
    let mut issues = Vec::new();
    let mut start = 0;
    while start < manifests.len() {
        let (_, hashes, chunk_sizes, _) = &manifests[start];
        if hashes.len() == chunk_sizes.len() && hashes.len() > CANDIDATE_BATCH_OCCURRENCES {
            issues.extend(candidate_large_manifest(&manifests[start], backend).await);
            start += 1;
            continue;
        }

        let mut occurrences = 0;
        let mut end = start;
        while end < manifests.len() {
            let (_, hashes, chunk_sizes, _) = &manifests[end];
            let next = if hashes.len() == chunk_sizes.len() {
                hashes.len()
            } else {
                0
            };
            if next > CANDIDATE_BATCH_OCCURRENCES
                || (occurrences > 0 && occurrences + next > CANDIDATE_BATCH_OCCURRENCES)
            {
                break;
            }
            occurrences += next;
            end += 1;
        }
        debug_assert!(end > start);
        issues.extend(candidate_batch(&manifests[start..end], backend).await);
        start = end;
    }
    issues
}

fn storage_hashes(manifests: &[Manifest]) -> Vec<&str> {
    let mut sizes: HashMap<&str, (), RandomState> = HashMap::default();
    for (_, hashes, chunk_sizes, _) in manifests {
        if hashes.len() == chunk_sizes.len() {
            for hash in hashes {
                // Keep phase 2 free of synthetic issues: both variants then
                // append exactly the same empty vector regardless of task
                // completion order. Phase-1 anomaly semantics remain gated.
                if !hash.starts_with("missing-") && !hash.starts_with("wrong-") {
                    sizes.entry(hash.as_str()).or_insert(());
                }
            }
        }
    }
    let mut hashes: Vec<&str> = sizes.into_keys().collect();
    hashes.sort_unstable();
    hashes
}

async fn phase_two(blob_hashes: &[&str], backend: &SimBackend) -> Vec<String> {
    stream::iter(blob_hashes.iter().copied())
        .map(|hash| async move {
            let mut issues = Vec::new();
            match backend.blob_size(hash).await {
                Some(actual) if actual != 256 => {
                    issues.push(format!(
                        "{hash}: size mismatch (expected: 256, actual: {actual})"
                    ));
                }
                None => {
                    issues.push(format!("{hash}: blob missing in backend"));
                    return issues;
                }
                Some(_) => {}
            }
            backend.hash_local_blob().await;
            issues
        })
        .buffer_unordered(CONCURRENCY)
        .flat_map(stream::iter)
        .collect()
        .await
}

fn fixture(manifests: usize, chunks: usize, unique: usize, anomalies: bool) -> Vec<Manifest> {
    let unique = unique.max(1);
    let mut rows = Vec::with_capacity(manifests + 3);
    for manifest in 0..manifests {
        let hashes: Vec<String> = (0..chunks)
            .map(|chunk| format!("chunk-{:058}", (manifest * chunks + chunk) % unique))
            .collect();
        rows.push((
            format!("file-{manifest:059}"),
            hashes,
            vec![256; chunks],
            (chunks * 256) as i64,
        ));
    }

    if !anomalies {
        return rows;
    }

    // Semantic gates: total mismatch, repeated wrong-size hash, repeated
    // missing hash, and a malformed manifest that must not trigger a call.
    rows.push((
        "sum-mismatch-file".into(),
        vec!["wrong-shared".into(), "wrong-shared".into()],
        vec![256, 257],
        1,
    ));
    rows.push((
        "missing-file".into(),
        vec!["missing-shared".into(), "missing-shared".into()],
        vec![256, 256],
        512,
    ));
    rows.push((
        "malformed-file".into(),
        vec!["missing-must-not-be-queried".into()],
        vec![],
        0,
    ));
    rows
}

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort_unstable();
    values[values.len() / 2]
}

async fn timed_once(
    candidate_mode: bool,
    manifests: &[Manifest],
    latency: Latency,
    repetitions: usize,
    full_method: bool,
    blob_hashes: &[&str],
) -> (Duration, usize, Vec<String>) {
    let backend = SimBackend::new(latency);
    let start = Instant::now();
    let mut issues = Vec::new();
    for _ in 0..repetitions {
        issues = if candidate_mode && !uses_serial_fast_path(manifests) {
            candidate(manifests, &backend).await
        } else {
            current(manifests, &backend).await
        };
        if full_method {
            issues.extend(phase_two(blob_hashes, &backend).await);
        }
        black_box(&issues);
    }
    (
        start.elapsed() / repetitions as u32,
        backend.calls() / repetitions,
        issues,
    )
}

async fn timed_pair(
    manifests: &[Manifest],
    latency: Latency,
    samples: usize,
    repetitions: usize,
    full_method: bool,
    blob_hashes: &[&str],
) -> (
    (Duration, usize, Vec<String>),
    (Duration, usize, Vec<String>),
) {
    let mut current_times = Vec::with_capacity(samples);
    let mut candidate_times = Vec::with_capacity(samples);
    let mut current_observation = None;
    let mut candidate_observation = None;
    for sample in 0..samples + 1 {
        let (current_run, candidate_run) = if sample % 2 == 0 {
            (
                timed_once(
                    false,
                    manifests,
                    latency,
                    repetitions,
                    full_method,
                    blob_hashes,
                )
                .await,
                timed_once(
                    true,
                    manifests,
                    latency,
                    repetitions,
                    full_method,
                    blob_hashes,
                )
                .await,
            )
        } else {
            let candidate = timed_once(
                true,
                manifests,
                latency,
                repetitions,
                full_method,
                blob_hashes,
            )
            .await;
            let current = timed_once(
                false,
                manifests,
                latency,
                repetitions,
                full_method,
                blob_hashes,
            )
            .await;
            (current, candidate)
        };
        if sample > 0 {
            current_times.push(current_run.0);
            candidate_times.push(candidate_run.0);
            current_observation = Some((current_run.1, current_run.2));
            candidate_observation = Some((candidate_run.1, candidate_run.2));
        }
    }
    let (current_calls, current_issues) = current_observation.expect("measured current run");
    let (candidate_calls, candidate_issues) =
        candidate_observation.expect("measured candidate run");
    (
        (median(current_times), current_calls, current_issues),
        (median(candidate_times), candidate_calls, candidate_issues),
    )
}

async fn raw_pair(
    manifests: &[Manifest],
    latency: Latency,
    samples: usize,
    repetitions: usize,
    full_method: bool,
    blob_hashes: &[&str],
) -> (Vec<f64>, Vec<f64>) {
    let mut current_times = Vec::with_capacity(samples);
    let mut candidate_times = Vec::with_capacity(samples);
    for sample in 0..samples + 1 {
        let (current, candidate) = if sample % 2 == 0 {
            (
                timed_once(
                    false,
                    manifests,
                    latency,
                    repetitions,
                    full_method,
                    blob_hashes,
                )
                .await,
                timed_once(
                    true,
                    manifests,
                    latency,
                    repetitions,
                    full_method,
                    blob_hashes,
                )
                .await,
            )
        } else {
            let candidate = timed_once(
                true,
                manifests,
                latency,
                repetitions,
                full_method,
                blob_hashes,
            )
            .await;
            let current = timed_once(
                false,
                manifests,
                latency,
                repetitions,
                full_method,
                blob_hashes,
            )
            .await;
            (current, candidate)
        };
        if sample > 0 {
            current_times.push(current.0.as_secs_f64() * 1e3);
            candidate_times.push(candidate.0.as_secs_f64() * 1e3);
        }
    }
    (current_times, candidate_times)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|value| value == "--tiny-raw") {
        let rows = fixture(1, 1, 1, false);
        let blob_hashes = storage_hashes(&rows);
        let latency = Latency::Remote(Duration::from_millis(4));
        let (phase_current, phase_candidate) =
            raw_pair(&rows, latency, 31, 8, false, &blob_hashes).await;
        let (full_current, full_candidate) =
            raw_pair(&rows, latency, 31, 8, true, &blob_hashes).await;
        println!("phase_current_ms={phase_current:?}");
        println!("phase_candidate_ms={phase_candidate:?}");
        println!("full_current_ms={full_current:?}");
        println!("full_candidate_ms={full_candidate:?}");
        return;
    }
    if args.get(1).is_some_and(|value| value == "--memory") {
        let mode = args.get(2).map(String::as_str).unwrap_or("candidate");
        assert!(matches!(mode, "current" | "candidate"));
        // 250k unique occurrences: large enough for process-level max RSS to
        // rise above allocator noise while keeping the probe quick.
        let rows = fixture(1_000, 250, 250_000, false);
        let backend = SimBackend::new(Latency::Immediate);
        let issues = if mode == "candidate" {
            candidate(&rows, &backend).await
        } else {
            current(&rows, &backend).await
        };
        black_box(&issues);
        println!(
            "mode={mode} manifests={} occurrences={} calls={} issues={}",
            rows.len(),
            250_000,
            backend.calls(),
            issues.len()
        );
        return;
    }

    let scenarios = [
        (
            "immediate_one_manifest_two",
            Latency::Immediate,
            1,
            2,
            2,
            false,
            51,
            10_000,
        ),
        (
            "immediate_two_manifests_one",
            Latency::Immediate,
            2,
            1,
            2,
            false,
            51,
            10_000,
        ),
        (
            "immediate_one_manifest_four",
            Latency::Immediate,
            1,
            4,
            4,
            false,
            51,
            10_000,
        ),
        (
            "local_tiny_empty",
            Latency::Local {
                metadata: Duration::from_micros(250),
                hash: Duration::from_millis(1),
            },
            0,
            0,
            1,
            false,
            101,
            10_000,
        ),
        (
            "local_tiny_single",
            Latency::Local {
                metadata: Duration::from_micros(250),
                hash: Duration::from_millis(1),
            },
            1,
            1,
            1,
            false,
            51,
            128,
        ),
        (
            "local_small_unique",
            Latency::Local {
                metadata: Duration::from_micros(250),
                hash: Duration::from_millis(1),
            },
            1,
            4,
            4,
            false,
            31,
            32,
        ),
        (
            "local_semantics",
            Latency::Local {
                metadata: Duration::from_micros(250),
                hash: Duration::from_millis(1),
            },
            2,
            4,
            4,
            true,
            15,
            1,
        ),
        (
            "local_shared",
            Latency::Local {
                metadata: Duration::from_micros(250),
                hash: Duration::from_millis(1),
            },
            64,
            8,
            32,
            false,
            7,
            1,
        ),
        (
            "local_unique",
            Latency::Local {
                metadata: Duration::from_micros(250),
                hash: Duration::from_millis(1),
            },
            32,
            8,
            256,
            false,
            7,
            1,
        ),
        (
            "local_hash_dominated",
            Latency::Local {
                metadata: Duration::from_micros(250),
                // Models large legacy/local blobs in phase 2. Both variants
                // pay exactly the same bounded-concurrency rehash cost.
                hash: Duration::from_millis(10),
            },
            64,
            8,
            32,
            false,
            3,
            1,
        ),
        (
            "remote_tiny_single",
            Latency::Remote(Duration::from_millis(4)),
            1,
            1,
            1,
            false,
            31,
            8,
        ),
        (
            "remote_shared",
            Latency::Remote(Duration::from_millis(4)),
            24,
            8,
            24,
            false,
            7,
            1,
        ),
        (
            "remote_unique",
            Latency::Remote(Duration::from_millis(4)),
            16,
            8,
            128,
            false,
            7,
            1,
        ),
    ];

    println!(
        "scenario,phase1_current_ms,phase1_candidate_ms,phase1_speedup,full_current_ms,\
         full_candidate_ms,full_speedup,phase1_current_calls,phase1_candidate_calls,\
         full_current_calls,full_candidate_calls,issues_equal"
    );
    let mut all_pass = true;
    for (name, latency, manifest_count, chunks, unique, anomalies, samples, repetitions) in
        scenarios
    {
        let rows = fixture(manifest_count, chunks, unique, anomalies);
        let extra_storage: Vec<String> = if name == "local_hash_dominated" {
            (0..128)
                .map(|index| format!("legacy-{index:057}"))
                .collect()
        } else {
            Vec::new()
        };
        let mut blob_hashes = storage_hashes(&rows);
        blob_hashes.extend(extra_storage.iter().map(String::as_str));
        let tiny = name.contains("tiny");
        let serial_fast_path = uses_serial_fast_path(&rows);
        let (
            (phase_current, phase_current_calls, phase_current_issues),
            (phase_candidate, phase_candidate_calls, phase_candidate_issues),
        ) = timed_pair(&rows, latency, samples, repetitions, false, &blob_hashes).await;
        let (
            (full_current, full_current_calls, full_current_issues),
            (full_candidate, full_candidate_calls, full_candidate_issues),
        ) = timed_pair(
            &rows,
            latency,
            samples,
            if tiny {
                repetitions.min(16)
            } else {
                repetitions
            },
            true,
            &blob_hashes,
        )
        .await;
        let equal = phase_current_issues == phase_candidate_issues
            && full_current_issues == full_candidate_issues;
        let phase_speedup = if phase_current.is_zero() && phase_candidate.is_zero() {
            1.0
        } else {
            phase_current.as_secs_f64() / phase_candidate.as_secs_f64()
        };
        let full_speedup = if full_current.is_zero() && full_candidate.is_zero() {
            1.0
        } else {
            full_current.as_secs_f64() / full_candidate.as_secs_f64()
        };
        // Tiny fast paths tolerate only timer noise; substantive cases must
        // be a strict win. Semantics and call-count reduction are hard gates.
        let phase_timing_pass = if phase_current < Duration::from_nanos(100) {
            // An empty Vec return is below the clock's useful resolution;
            // permit at most twenty nanoseconds of measurement noise.
            phase_candidate <= phase_current + Duration::from_nanos(20)
        } else if serial_fast_path {
            phase_speedup >= 0.95
        } else {
            phase_speedup > 1.0
        };
        let full_timing_pass = if full_current < Duration::from_nanos(100) {
            full_candidate <= full_current + Duration::from_nanos(20)
        } else if serial_fast_path {
            full_speedup >= 0.95
        } else {
            full_speedup > 1.0
        };
        let calls_pass = phase_candidate_calls <= phase_current_calls
            && full_candidate_calls <= full_current_calls;
        all_pass &= equal && phase_timing_pass && full_timing_pass && calls_pass;
        println!(
            "{name},{:.6},{:.6},{phase_speedup:.3},{:.6},{:.6},{full_speedup:.3},\
             {phase_current_calls},{phase_candidate_calls},{full_current_calls},\
             {full_candidate_calls},{equal}",
            phase_current.as_secs_f64() * 1e3,
            phase_candidate.as_secs_f64() * 1e3,
            full_current.as_secs_f64() * 1e3,
            full_candidate.as_secs_f64() * 1e3,
        );
    }
    assert!(all_pass, "candidate failed a correctness/performance gate");
}
