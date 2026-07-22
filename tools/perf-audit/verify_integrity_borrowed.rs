//! A/B/C for the phase-1 integrity verifier's bounded result table.
//!
//! `historical` probes every manifest occurrence serially. `owned` models the
//! current accepted 256-occurrence/concurrency-16 candidate exactly: each
//! distinct key is cloned into the map and cloned again into the work vector.
//! `borrowed` changes only those scratch keys to `&str`. Tiny stores retain the
//! exact historical loop through four valid occurrences in both candidates.

use foldhash::quality::RandomState;
use futures::stream::{self, StreamExt};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::future::Future;
use std::hint::black_box;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const PRODUCTION_CONCURRENCY: usize = 16;
const WINDOW: usize = 256;
const SERIAL_FAST_PATH_OCCURRENCES: usize = 4;
static CANDIDATE_CONCURRENCY: AtomicUsize = AtomicUsize::new(PRODUCTION_CONCURRENCY);
type Manifest = (String, Vec<String>, Vec<i64>, i64);
type OwnedSizes = HashMap<String, Option<u64>, RandomState>;
type BorrowedSizes<'a> = HashMap<&'a str, Option<u64>, RandomState>;
type AuditBoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

struct TrackingAllocator;

static LIVE_ALLOCATED: AtomicUsize = AtomicUsize::new(0);
static PEAK_ALLOCATED: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

#[inline]
fn update_peak(candidate: usize) {
    let mut peak = PEAK_ALLOCATED.load(Ordering::Relaxed);
    while candidate > peak {
        match PEAK_ALLOCATED.compare_exchange_weak(
            peak,
            candidate,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(observed) => peak = observed,
        }
    }
}

// SAFETY: every operation delegates to `System` with the original pointer and
// layout; the counters are diagnostic and do not affect allocation semantics.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let live = LIVE_ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            update_peak(live);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE_ALLOCATED.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, old: Layout, new_size: usize) -> *mut u8 {
        let new_pointer = unsafe { System.realloc(pointer, old, new_size) };
        if !new_pointer.is_null() {
            if new_size >= old.size() {
                let growth = new_size - old.size();
                let live = LIVE_ALLOCATED.fetch_add(growth, Ordering::Relaxed) + growth;
                update_peak(live);
            } else {
                LIVE_ALLOCATED.fetch_sub(old.size() - new_size, Ordering::Relaxed);
            }
        }
        new_pointer
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Historical,
    Owned,
    Borrowed,
    Sorted,
}

impl Mode {
    fn parse(value: &str) -> Self {
        match value {
            "historical" => Self::Historical,
            "owned" => Self::Owned,
            "borrowed" => Self::Borrowed,
            "sorted" => Self::Sorted,
            _ => panic!("mode must be historical, owned, borrowed, or sorted"),
        }
    }
}

#[derive(Clone, Copy)]
enum Latency {
    Immediate,
    Local { metadata: Duration, hash: Duration },
    Remote(Duration),
    RealFs(&'static Path),
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

    fn blob_size<'a>(&'a self, hash: &'a str) -> AuditBoxFut<'a, Option<u64>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            match self.latency {
                Latency::Immediate => {}
                Latency::Local { metadata, .. } => tokio::time::sleep(metadata).await,
                Latency::Remote(delay) => tokio::time::sleep(delay).await,
                Latency::RealFs(root) => {
                    return tokio::fs::metadata(root.join(hash))
                        .await
                        .ok()
                        .map(|metadata| metadata.len());
                }
            }
            if hash.starts_with("missing-") {
                None
            } else if hash.starts_with("wrong-") {
                Some(999)
            } else {
                Some(256)
            }
        })
    }

    async fn hash_local_blob(&self, blob_hash: &str) {
        match self.latency {
            Latency::Local { hash, .. } => tokio::time::sleep(hash).await,
            Latency::RealFs(root) => {
                black_box(tokio::fs::read(root.join(blob_hash)).await.ok());
            }
            Latency::Immediate | Latency::Remote(_) => {}
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

#[inline]
fn label(value: &str) -> &str {
    &value[..value.len().min(12)]
}

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

async fn historical(manifests: &[Manifest], backend: &SimBackend) -> Vec<String> {
    let mut issues = Vec::new();
    for (file_hash, hashes, expected_sizes, total_size) in manifests {
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

fn replay_owned(manifests: &[Manifest], sizes: &OwnedSizes) -> Vec<String> {
    replay(manifests, |hash| sizes.get(hash).copied().flatten())
}

fn replay_borrowed(manifests: &[Manifest], sizes: &BorrowedSizes<'_>) -> Vec<String> {
    replay(manifests, |hash| sizes.get(hash).copied().flatten())
}

fn replay<F>(manifests: &[Manifest], mut size_of: F) -> Vec<String>
where
    F: FnMut(&str) -> Option<u64>,
{
    let mut issues = Vec::new();
    for (file_hash, hashes, expected_sizes, total_size) in manifests {
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

async fn fill_owned(sizes: OwnedSizes, backend: &SimBackend) -> OwnedSizes {
    let hashes: Vec<String> = sizes.keys().cloned().collect();
    stream::iter(hashes)
        .map(|hash| async move {
            let size = backend.blob_size(&hash).await;
            (hash, size)
        })
        .buffer_unordered(CANDIDATE_CONCURRENCY.load(Ordering::Relaxed))
        .fold(sizes, |mut sizes, (hash, size)| async move {
            sizes.insert(hash, size);
            sizes
        })
        .await
}

async fn fill_borrowed<'a>(sizes: BorrowedSizes<'a>, backend: &SimBackend) -> BorrowedSizes<'a> {
    let hashes: Vec<&'a str> = sizes.keys().copied().collect();
    stream::iter(hashes)
        .map(|hash| async move {
            let size = backend.blob_size(hash).await;
            (hash, size)
        })
        .buffer_unordered(CANDIDATE_CONCURRENCY.load(Ordering::Relaxed))
        .fold(sizes, |mut sizes, (hash, size)| async move {
            sizes.insert(hash, size);
            sizes
        })
        .await
}

async fn owned_batch(manifests: &[Manifest], backend: &SimBackend) -> Vec<String> {
    let mut sizes = OwnedSizes::default();
    for (_, hashes, expected_sizes, _) in manifests {
        if hashes.len() == expected_sizes.len() {
            for hash in hashes {
                sizes.entry(hash.clone()).or_insert(None);
            }
        }
    }
    let sizes = fill_owned(sizes, backend).await;
    replay_owned(manifests, &sizes)
}

async fn borrowed_batch(manifests: &[Manifest], backend: &SimBackend) -> Vec<String> {
    let mut sizes = BorrowedSizes::default();
    for (_, hashes, expected_sizes, _) in manifests {
        if hashes.len() == expected_sizes.len() {
            for hash in hashes {
                sizes.entry(hash.as_str()).or_insert(None);
            }
        }
    }
    let sizes = fill_borrowed(sizes, backend).await;
    replay_borrowed(manifests, &sizes)
}

async fn sorted_batch(manifests: &[Manifest], backend: &SimBackend) -> Vec<String> {
    let mut hashes = Vec::new();
    for (_, manifest_hashes, expected_sizes, _) in manifests {
        if manifest_hashes.len() == expected_sizes.len() {
            hashes.extend(manifest_hashes.iter().map(String::as_str));
        }
    }
    hashes.sort_unstable();
    hashes.dedup();
    let mut values = vec![None; hashes.len()];
    let concurrency = CANDIDATE_CONCURRENCY.load(Ordering::Relaxed).max(1);
    let mut pending = futures::stream::FuturesUnordered::new();
    let mut next = 0usize;
    while next < hashes.len() || !pending.is_empty() {
        while next < hashes.len() && pending.len() < concurrency {
            let index = next;
            let hash = hashes[index];
            pending.push(async move {
                let size = backend.blob_size(hash).await;
                (index, size)
            });
            next += 1;
        }
        if let Some((index, size)) = pending.next().await {
            values[index] = size;
        }
    }
    replay(manifests, |hash| {
        hashes
            .binary_search(&hash)
            .ok()
            .and_then(|index| values[index])
    })
}

async fn windowed(manifests: &[Manifest], backend: &SimBackend, mode: Mode) -> Vec<String> {
    let mut issues = Vec::new();
    let mut start = 0;
    while start < manifests.len() {
        let (_, hashes, expected_sizes, _) = &manifests[start];
        if hashes.len() == expected_sizes.len() && hashes.len() > WINDOW {
            let (file_hash, hashes, expected_sizes, total_size) = &manifests[start];
            let file_label = label(file_hash);
            let sum: i64 = expected_sizes.iter().sum();
            if sum != *total_size {
                issues.push(format!(
                    "Manifest {file_label}: total_size {total_size} != sum of chunk_sizes {sum}"
                ));
            }
            for offset in (0..hashes.len()).step_by(WINDOW) {
                let end = (offset + WINDOW).min(hashes.len());
                let synthetic = (
                    file_hash.clone(),
                    hashes[offset..end].to_vec(),
                    expected_sizes[offset..end].to_vec(),
                    expected_sizes[offset..end].iter().sum(),
                );
                let batch = std::slice::from_ref(&synthetic);
                let mut batch_issues = match mode {
                    Mode::Owned => owned_batch(batch, backend).await,
                    Mode::Borrowed => borrowed_batch(batch, backend).await,
                    Mode::Sorted => sorted_batch(batch, backend).await,
                    Mode::Historical => unreachable!(),
                };
                // Slice replay must not repeat a total-size issue already emitted.
                batch_issues.retain(|issue| !issue.contains("sum of chunk_sizes"));
                issues.extend(batch_issues);
            }
            start += 1;
            continue;
        }

        let mut occurrences = 0;
        let mut end = start;
        while end < manifests.len() {
            let (_, hashes, expected_sizes, _) = &manifests[end];
            let next = if hashes.len() == expected_sizes.len() {
                hashes.len()
            } else {
                0
            };
            if next > WINDOW || (occurrences > 0 && occurrences + next > WINDOW) {
                break;
            }
            occurrences += next;
            end += 1;
        }
        debug_assert!(end > start);
        let batch = &manifests[start..end];
        issues.extend(match mode {
            Mode::Owned => owned_batch(batch, backend).await,
            Mode::Borrowed => borrowed_batch(batch, backend).await,
            Mode::Sorted => sorted_batch(batch, backend).await,
            Mode::Historical => unreachable!(),
        });
        start = end;
    }
    issues
}

async fn verify(manifests: &[Manifest], backend: &SimBackend, mode: Mode) -> Vec<String> {
    if mode == Mode::Historical || uses_serial_fast_path(manifests) {
        historical(manifests, backend).await
    } else {
        windowed(manifests, backend, mode).await
    }
}

fn fixture(manifests: usize, chunks: usize, unique: usize, anomalies: bool) -> Vec<Manifest> {
    let unique = unique.max(1);
    let mut rows = Vec::with_capacity(manifests + usize::from(anomalies) * 3);
    for manifest in 0..manifests {
        let hashes = (0..chunks)
            .map(|chunk| audit_hash((manifest * chunks + chunk) % unique))
            .collect();
        rows.push((
            format!("file-{manifest:059}"),
            hashes,
            vec![256; chunks],
            (chunks * 256) as i64,
        ));
    }
    if anomalies {
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
    }
    rows
}

fn audit_hash(index: usize) -> String {
    fn mix(mut value: u64) -> u64 {
        value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
    let a = mix(index as u64);
    let b = mix(a);
    let c = mix(b);
    let d = mix(c);
    format!("{a:016x}{b:016x}{c:016x}{d:016x}")
}

fn mark_missing(rows: &mut [Manifest], every: usize) {
    let mut occurrence = 0usize;
    for (_, hashes, _, _) in rows {
        for hash in hashes {
            if occurrence.is_multiple_of(every) {
                *hash = format!("missing-{hash}");
            }
            occurrence += 1;
        }
    }
}

fn populate_real_fs(root: &Path, scenarios: &[Vec<Manifest>]) {
    std::fs::create_dir_all(root).expect("create real-filesystem fixture directory");
    for rows in scenarios {
        for (_, hashes, expected_sizes, _) in rows {
            if hashes.len() != expected_sizes.len() {
                continue;
            }
            for hash in hashes {
                if hash.starts_with("missing-") {
                    continue;
                }
                let file = std::fs::File::create(root.join(hash)).expect("create fixture blob");
                let length = if hash.starts_with("wrong-") { 999 } else { 256 };
                file.set_len(length).expect("size fixture blob");
            }
        }
    }
}

fn storage_hashes(manifests: &[Manifest]) -> Vec<&str> {
    let mut unique: HashMap<&str, (), RandomState> = HashMap::default();
    for (_, hashes, expected_sizes, _) in manifests {
        if hashes.len() == expected_sizes.len() {
            for hash in hashes {
                if !hash.starts_with("missing-") && !hash.starts_with("wrong-") {
                    unique.entry(hash).or_insert(());
                }
            }
        }
    }
    let mut hashes: Vec<&str> = unique.into_keys().collect();
    hashes.sort_unstable();
    hashes
}

async fn phase_two(hashes: &[&str], backend: &SimBackend) {
    stream::iter(hashes.iter().copied())
        .map(|hash| async move {
            black_box(backend.blob_size(hash).await);
            backend.hash_local_blob(hash).await;
        })
        .buffer_unordered(PRODUCTION_CONCURRENCY)
        .collect::<Vec<()>>()
        .await;
}

async fn phase_two_generated(count: usize, backend: &SimBackend) {
    stream::iter(0..count)
        .map(|index| async move {
            let hash = audit_hash(index);
            black_box(backend.blob_size(&hash).await);
            backend.hash_local_blob(&hash).await;
        })
        .buffer_unordered(PRODUCTION_CONCURRENCY)
        .collect::<Vec<()>>()
        .await;
}

#[derive(Clone, Copy)]
struct Observation {
    phase: Duration,
    full: Duration,
    phase_calls: usize,
    full_calls: usize,
    issue_checksum: usize,
}

async fn observe(
    mode: Mode,
    manifests: &[Manifest],
    latency: Latency,
    repetitions: usize,
    storage: &[&str],
) -> Observation {
    let phase_backend = SimBackend::new(latency);
    let start = Instant::now();
    let mut issues = Vec::new();
    for _ in 0..repetitions {
        issues = verify(manifests, &phase_backend, mode).await;
        black_box(&issues);
    }
    let phase = start.elapsed() / repetitions as u32;

    let full_backend = SimBackend::new(latency);
    let start = Instant::now();
    for _ in 0..repetitions {
        issues = verify(manifests, &full_backend, mode).await;
        phase_two(storage, &full_backend).await;
        black_box(&issues);
    }
    let full = start.elapsed() / repetitions as u32;
    Observation {
        phase,
        full,
        phase_calls: phase_backend.calls() / repetitions,
        full_calls: full_backend.calls() / repetitions,
        issue_checksum: issues.iter().map(String::len).sum(),
    }
}

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort_unstable();
    values[values.len() / 2]
}

async fn measure(
    manifests: &[Manifest],
    latency: Latency,
    samples: usize,
    repetitions: usize,
) -> [Observation; 4] {
    let storage = storage_hashes(manifests);
    let modes = [Mode::Historical, Mode::Owned, Mode::Borrowed, Mode::Sorted];
    let mut phase = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    let mut full = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    let mut last = [None, None, None, None];
    for sample in 0..=samples {
        for offset in 0..4 {
            let index = (sample + offset) % 4;
            let observation =
                observe(modes[index], manifests, latency, repetitions, &storage).await;
            if sample > 0 {
                phase[index].push(observation.phase);
                full[index].push(observation.full);
                last[index] = Some(observation);
            }
        }
    }
    std::array::from_fn(|index| {
        let mut observation = last[index].expect("at least one measured sample");
        observation.phase = median(std::mem::take(&mut phase[index]));
        observation.full = median(std::mem::take(&mut full[index]));
        observation
    })
}

fn print_header() {
    println!(
        "scenario,historical_phase_ms,owned_phase_ms,borrowed_phase_ms,sorted_phase_ms,\
         borrowed_vs_owned_phase,sorted_vs_owned_phase,historical_full_ms,owned_full_ms,\
         borrowed_full_ms,sorted_full_ms,borrowed_vs_owned_full,sorted_vs_owned_full,\
         calls_historical,calls_owned,calls_borrowed,calls_sorted,full_calls_historical,\
         full_calls_owned,full_calls_borrowed,full_calls_sorted,issues_equal"
    );
}

async fn run_scenario(
    name: &str,
    latency: Latency,
    rows: &[Manifest],
    samples: usize,
    repetitions: usize,
) {
    let historical_gate =
        verify(rows, &SimBackend::new(Latency::Immediate), Mode::Historical).await;
    let owned_gate = verify(rows, &SimBackend::new(Latency::Immediate), Mode::Owned).await;
    let borrowed_gate = verify(rows, &SimBackend::new(Latency::Immediate), Mode::Borrowed).await;
    let sorted_gate = verify(rows, &SimBackend::new(Latency::Immediate), Mode::Sorted).await;
    assert_eq!(
        historical_gate, owned_gate,
        "owned issue gate failed for {name}"
    );
    assert_eq!(
        historical_gate, borrowed_gate,
        "borrowed issue gate failed for {name}"
    );
    assert_eq!(
        historical_gate, sorted_gate,
        "sorted issue gate failed for {name}"
    );
    let observations = measure(rows, latency, samples, repetitions).await;
    let [historical, owned, borrowed, sorted] = observations;
    let equal = historical.issue_checksum == owned.issue_checksum
        && historical.issue_checksum == borrowed.issue_checksum
        && historical.issue_checksum == sorted.issue_checksum;
    assert!(equal, "issue gate failed for {name}");
    assert_eq!(owned.phase_calls, borrowed.phase_calls);
    assert_eq!(owned.phase_calls, sorted.phase_calls);
    assert_eq!(owned.full_calls, borrowed.full_calls);
    assert_eq!(owned.full_calls, sorted.full_calls);
    assert!(owned.phase_calls <= historical.phase_calls);
    println!(
        "{name},{:.6},{:.6},{:.6},{:.6},{:.3},{:.3},{:.6},{:.6},{:.6},{:.6},{:.3},{:.3},{},{},{},{},{},{},{},{},{}",
        historical.phase.as_secs_f64() * 1e3,
        owned.phase.as_secs_f64() * 1e3,
        borrowed.phase.as_secs_f64() * 1e3,
        sorted.phase.as_secs_f64() * 1e3,
        owned.phase.as_secs_f64() / borrowed.phase.as_secs_f64(),
        owned.phase.as_secs_f64() / sorted.phase.as_secs_f64(),
        historical.full.as_secs_f64() * 1e3,
        owned.full.as_secs_f64() * 1e3,
        borrowed.full.as_secs_f64() * 1e3,
        sorted.full.as_secs_f64() * 1e3,
        owned.full.as_secs_f64() / borrowed.full.as_secs_f64(),
        owned.full.as_secs_f64() / sorted.full.as_secs_f64(),
        historical.phase_calls,
        owned.phase_calls,
        borrowed.phase_calls,
        sorted.phase_calls,
        historical.full_calls,
        owned.full_calls,
        borrowed.full_calls,
        sorted.full_calls,
        equal,
    );
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let candidate_concurrency = std::env::var("OXICLOUD_AUDIT_CONCURRENCY")
        .ok()
        .map(|value| value.parse::<usize>().expect("numeric concurrency"))
        .unwrap_or(PRODUCTION_CONCURRENCY);
    assert!(matches!(candidate_concurrency, 4 | 8 | 16));
    CANDIDATE_CONCURRENCY.store(candidate_concurrency, Ordering::Relaxed);
    if args.get(1).is_some_and(|arg| arg == "--memory") {
        let mode = Mode::parse(args.get(2).map(String::as_str).unwrap_or("borrowed"));
        let full_method = args.iter().any(|arg| arg == "full" || arg == "full-drop");
        let drop_manifests = args.iter().any(|arg| arg == "full-drop");
        let rows = fixture(1_000, 250, 250_000, false);
        let manifest_count = rows.len();
        let backend = SimBackend::new(Latency::Immediate);
        let live_before = LIVE_ALLOCATED.load(Ordering::Relaxed);
        PEAK_ALLOCATED.store(live_before, Ordering::Relaxed);
        let issues = verify(&rows, &backend, mode).await;
        let phase_peak = PEAK_ALLOCATED.load(Ordering::Relaxed);
        black_box(&issues);
        if drop_manifests {
            drop(rows);
        }
        let live_before_phase_two = LIVE_ALLOCATED.load(Ordering::Relaxed);
        if full_method {
            phase_two_generated(250_000, &backend).await;
        }
        let full_peak = PEAK_ALLOCATED.load(Ordering::Relaxed);
        println!(
            "mode={mode:?} concurrency={candidate_concurrency} full={full_method} drop_manifests={drop_manifests} manifests={manifest_count} occurrences=250000 calls={} issues={} live_before={} phase_peak={} phase_scratch_peak={} live_before_phase_two={} full_peak={}",
            backend.calls(),
            issues.len(),
            live_before,
            phase_peak,
            phase_peak.saturating_sub(live_before),
            live_before_phase_two,
            full_peak,
        );
        return;
    }

    if args.iter().any(|arg| arg == "--real-fs") {
        let mut tiny_missing = fixture(2, 1, 2, false);
        mark_missing(&mut tiny_missing, 2);
        let shared = fixture(64, 8, 32, false);
        let unique = fixture(32, 8, 256, false);
        let mut mixed = fixture(32, 8, 256, false);
        mark_missing(&mut mixed, 17);
        let real_rows = vec![fixture(1, 2, 2, false), tiny_missing, shared, unique, mixed];
        let path = std::env::temp_dir().join(format!(
            "oxicloud-integrity-real-fs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        populate_real_fs(&path, &real_rows);
        let leaked_root: &'static Path = Box::leak(path.clone().into_boxed_path());
        print_header();
        let names = [
            "real_tiny_1x2",
            "real_tiny_missing_2x1",
            "real_shared",
            "real_unique",
            "real_mixed_unique",
        ];
        for (name, rows) in names.into_iter().zip(real_rows.iter()) {
            let repetitions = if name.starts_with("real_tiny") {
                128
            } else {
                1
            };
            run_scenario(name, Latency::RealFs(leaked_root), rows, 31, repetitions).await;
        }
        std::fs::remove_dir_all(path).expect("remove real-filesystem fixture directory");
        return;
    }

    let scenarios = [
        (
            "immediate_1x2",
            Latency::Immediate,
            1,
            2,
            2,
            false,
            51,
            10_000,
        ),
        (
            "immediate_2x1",
            Latency::Immediate,
            2,
            1,
            2,
            false,
            51,
            10_000,
        ),
        (
            "immediate_1x4",
            Latency::Immediate,
            1,
            4,
            4,
            false,
            51,
            10_000,
        ),
        (
            "cpu_shared",
            Latency::Immediate,
            64,
            8,
            32,
            false,
            31,
            1_000,
        ),
        (
            "cpu_unique",
            Latency::Immediate,
            32,
            8,
            256,
            false,
            31,
            1_000,
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

    print_header();
    let remote_only = args.iter().any(|arg| arg == "--remote-only");
    for (name, latency, manifests, chunks, unique, anomalies, samples, repetitions) in scenarios {
        if remote_only && !name.starts_with("remote_") {
            continue;
        }
        let rows = fixture(manifests, chunks, unique, anomalies);
        run_scenario(name, latency, &rows, samples, repetitions).await;
    }
}
