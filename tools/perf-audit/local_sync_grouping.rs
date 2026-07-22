//! Standalone A/B for allocation work in LocalBlobBackend::sync_blobs.
//!
//! Compile directly with rustc so this audit is independent of Cargo's
//! benchmark targets:
//!   rustc --edition 2024 -O tools/perf-audit/local_sync_grouping.rs -o /tmp/local-sync-grouping

use std::hint::black_box;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant};

const CONCURRENCY: usize = 16;

type EmptyPrepFuture = Pin<Box<dyn Future<Output = usize>>>;

// Historical empty-call shape: build the paths Vec, then discover emptiness
// inside the boxed future.
#[inline(never)]
fn current_empty_prep(root: &Path, hashes: &[String]) -> EmptyPrepFuture {
    let paths: Vec<PathBuf> = hashes
        .iter()
        .map(|hash| root.join(&hash[..2]).join(format!("{hash}.blob")))
        .collect();
    Box::pin(async move {
        if paths.is_empty() {
            0
        } else {
            paths.len()
        }
    })
}

// Accepted candidate shape: an empty durability sweep has no observable work,
// so return a capture-free ready future before allocating/preparing anything.
#[inline(never)]
fn fast_empty_prep(_root: &Path, hashes: &[String]) -> EmptyPrepFuture {
    if hashes.is_empty() {
        return Box::pin(async { 0 });
    }
    let paths: Vec<PathBuf> = hashes.iter().map(PathBuf::from).collect();
    Box::pin(async move { paths.len() })
}

fn paths(count: usize) -> Vec<PathBuf> {
    (0..count)
        .map(|i| {
            let prefix = format!("{:02x}", i & 255);
            let hash = format!("{prefix}{:062x}", i);
            Path::new("/tmp/oxicloud/.blobs")
                .join(prefix)
                .join(format!("{hash}.blob"))
        })
        .collect()
}

// Exact grouping shape currently used before spawning the blocking tasks.
fn current_groups(paths: Vec<PathBuf>) -> Vec<Vec<PathBuf>> {
    let group_size = paths.len().div_ceil(CONCURRENCY);
    paths.chunks(group_size).map(<[PathBuf]>::to_vec).collect()
}

// Candidate: the caller already owns the Vec, so move each PathBuf into its
// task group instead of cloning every path and keeping the original alive.
fn moved_groups(paths: Vec<PathBuf>) -> Vec<Vec<PathBuf>> {
    let group_size = paths.len().div_ceil(CONCURRENCY);
    let mut source = paths.into_iter();
    let mut groups = Vec::with_capacity(CONCURRENCY.min(source.len()));
    loop {
        let group: Vec<PathBuf> = source.by_ref().take(group_size).collect();
        if group.is_empty() {
            break;
        }
        groups.push(group);
    }
    groups
}

// Exact current distinct-parent preparation.
fn current_dirs(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = paths
        .iter()
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect();
    dirs.sort_unstable();
    dirs.dedup();
    dirs
}

fn hex_prefix_symbol(byte: u8) -> Option<usize> {
    match byte {
        b'0'..=b'9' => Some((byte - b'0') as usize),
        b'a'..=b'f' => Some((byte - b'a' + 10) as usize),
        b'A'..=b'F' => Some((byte - b'A' + 16) as usize),
        _ => None,
    }
}

// Candidate: exact-case hex prefixes fit in a tiny fixed bitmap.  Case must
// not be folded because `af/` and `AF/` differ on a case-sensitive filesystem.
fn prefix_dirs(root: &Path, hashes: &[String], paths: &[PathBuf]) -> Vec<PathBuf> {
    if hashes.len() == 1 {
        let mut dirs = Vec::with_capacity(1);
        if let Some(parent) = paths[0].parent() {
            dirs.push(parent.to_owned());
        }
        return dirs;
    }
    let mut seen = [false; 22 * 22];
    let mut dirs = Vec::with_capacity(256.min(hashes.len()));
    for hash in hashes {
        let bytes = hash.as_bytes();
        let slot = hex_prefix_symbol(bytes[0])
            .zip(hex_prefix_symbol(bytes[1]))
            .map(|(high, low)| high * 22 + low);
        if slot.is_none_or(|slot| !std::mem::replace(&mut seen[slot], true)) {
            dirs.push(root.join(&hash[..2]));
        }
    }
    dirs
}

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort_unstable();
    values[values.len() / 2]
}

fn measure_pair<T>(
    samples: usize,
    mut current: impl FnMut() -> T,
    mut candidate: impl FnMut() -> T,
) -> (Duration, Duration) {
    for _ in 0..3 {
        black_box(current());
        black_box(candidate());
    }
    let mut current_times = Vec::with_capacity(samples);
    let mut candidate_times = Vec::with_capacity(samples);
    for sample in 0..samples {
        let run = |operation: &mut dyn FnMut() -> T, times: &mut Vec<Duration>| {
            let start = Instant::now();
            black_box(operation());
            times.push(start.elapsed());
        };
        // Alternate order so allocator/cache/thermal drift cannot consistently
        // favour either implementation.
        if sample % 2 == 0 {
            run(&mut current, &mut current_times);
            run(&mut candidate, &mut candidate_times);
        } else {
            run(&mut candidate, &mut candidate_times);
            run(&mut current, &mut current_times);
        }
    }
    (median(current_times), median(candidate_times))
}

fn main() {
    let empty: Vec<String> = Vec::new();
    let empty_repetitions = 100_000;
    let (current_empty, fast_empty) = measure_pair(
        31,
        || {
            for _ in 0..empty_repetitions {
                let _ = black_box(current_empty_prep(
                    Path::new("/tmp/oxicloud/.blobs"),
                    &empty,
                ));
            }
        },
        || {
            for _ in 0..empty_repetitions {
                let _ = black_box(fast_empty_prep(Path::new("/tmp/oxicloud/.blobs"), &empty));
            }
        },
    );
    println!(
        "empty,current_ns,fast_return_ns,speedup\n0,{:.3},{:.3},{:.2}",
        current_empty.as_secs_f64() * 1e9 / empty_repetitions as f64,
        fast_empty.as_secs_f64() * 1e9 / empty_repetitions as f64,
        current_empty.as_secs_f64() / fast_empty.as_secs_f64(),
    );
    if std::env::args().any(|argument| argument == "--empty") {
        return;
    }

    let mixed_case = vec![format!("af{}", "0".repeat(62)), format!("aF{}", "0".repeat(62))];
    let mixed_paths: Vec<PathBuf> = mixed_case
        .iter()
        .map(|hash| Path::new("/tmp/oxicloud/.blobs").join(&hash[..2]).join(hash))
        .collect();
    let mut current_mixed = current_dirs(&mixed_paths);
    let mut candidate_mixed = prefix_dirs(
        Path::new("/tmp/oxicloud/.blobs"),
        &mixed_case,
        &mixed_paths,
    );
    current_mixed.sort_unstable();
    candidate_mixed.sort_unstable();
    assert_eq!(current_mixed, candidate_mixed);

    println!("count,current_group_us,moved_group_us,group_speedup,current_dirs_us,prefix_dirs_us,dirs_speedup");
    for count in [1, 8, 32, 128, 400, 1_600, 10_000, 100_000] {
        let source = paths(count);
        let hashes: Vec<String> = (0..count)
            .map(|i| format!("{:02x}{:062x}", i & 255, i))
            .collect();

        let current_check = current_groups(source.clone());
        let moved_check = moved_groups(source.clone());
        assert_eq!(
            current_check.iter().flatten().collect::<Vec<_>>(),
            moved_check.iter().flatten().collect::<Vec<_>>()
        );
        let current_dir_check = current_dirs(&source);
        let mut candidate_dir_check =
            prefix_dirs(Path::new("/tmp/oxicloud/.blobs"), &hashes, &source);
        candidate_dir_check.sort_unstable();
        assert_eq!(current_dir_check, candidate_dir_check);

        // Accumulate tiny cases inside each timed sample so sub-microsecond
        // operations are not decided by one timer tick. Report normalized
        // per-operation medians below.
        let repetitions = match count {
            1 => 10_000,
            8 => 1_000,
            32 => 250,
            _ => 1,
        };
        let (current_group, moved_group) = measure_pair(
            31,
            || {
                for _ in 0..repetitions {
                    black_box(current_groups(source.clone()));
                }
            },
            || {
                for _ in 0..repetitions {
                    black_box(moved_groups(source.clone()));
                }
            },
        );
        let (current_dir, candidate_dir) = measure_pair(
            31,
            || {
                for _ in 0..repetitions {
                    black_box(current_dirs(&source));
                }
            },
            || {
                for _ in 0..repetitions {
                    black_box(prefix_dirs(
                        Path::new("/tmp/oxicloud/.blobs"),
                        &hashes,
                        &source,
                    ));
                }
            },
        );

        let divisor = repetitions as f64;
        let current_group_us = current_group.as_secs_f64() * 1e6 / divisor;
        let moved_group_us = moved_group.as_secs_f64() * 1e6 / divisor;
        let current_dir_us = current_dir.as_secs_f64() * 1e6 / divisor;
        let candidate_dir_us = candidate_dir.as_secs_f64() * 1e6 / divisor;
        println!(
            "{count},{current_group_us:.3},{moved_group_us:.3},{:.2},{current_dir_us:.3},{candidate_dir_us:.3},{:.2}",
            current_group_us / moved_group_us,
            current_dir_us / candidate_dir_us,
        );
    }
}
