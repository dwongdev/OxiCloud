//! Reproducible local-file A/B for the cached blob range length calculation.
//!
//! This exercises the hot-cache filesystem shape (open, seek, limit, read)
//! while changing only the historical inclusive-end arithmetic versus the
//! `BlobStorageBackend` contract's exclusive end. It intentionally lives
//! outside production tests and outside `benches/`.

use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::hint::black_box;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const DATA: &[u8] = b"abcdef";
const START: u64 = 1;
const END_EXCLUSIVE: u64 = 3;

#[derive(Clone, Copy)]
enum Algorithm {
    HistoricalInclusive,
    CorrectedExclusive,
}

#[derive(Default)]
struct Samples {
    elapsed_ns: Vec<u128>,
    bytes: u64,
    checksum: u64,
}

struct Fixture {
    dir: PathBuf,
    blob: PathBuf,
}

impl Fixture {
    fn create() -> Result<Self, Box<dyn Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let dir = env::temp_dir().join(format!(
            "oxicloud-cached-range-ab-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&dir)?;
        let blob = dir.join("fixture.blob");
        fs::write(&blob, DATA)?;
        Ok(Self { dir, blob })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn parse_count(name: &str, default: usize) -> Result<usize, Box<dyn Error>> {
    match env::var(name) {
        Ok(raw) => {
            let value = raw.parse::<usize>()?;
            if value == 0 {
                return Err(format!("{name} must be greater than zero").into());
            }
            Ok(value)
        }
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error.into()),
    }
}

fn read_range(path: &Path, algorithm: Algorithm) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(START))?;
    let take_len = match algorithm {
        Algorithm::HistoricalInclusive => END_EXCLUSIVE - START + 1,
        Algorithm::CorrectedExclusive => END_EXCLUSIVE.saturating_sub(START),
    };
    let mut output = Vec::with_capacity(take_len as usize);
    file.take(take_len).read_to_end(&mut output)?;
    black_box(&output);
    Ok(output)
}

fn record(path: &Path, algorithm: Algorithm, samples: &mut Samples) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let output = read_range(path, algorithm)?;
    samples.elapsed_ns.push(started.elapsed().as_nanos());
    samples.bytes += output.len() as u64;
    samples.checksum = samples
        .checksum
        .wrapping_add(output.iter().map(|byte| u64::from(*byte)).sum::<u64>());
    Ok(())
}

fn percentile_us(samples: &mut [u128], percentile: usize) -> f64 {
    samples.sort_unstable();
    let rank = ((samples.len() - 1) * percentile) / 100;
    samples[rank] as f64 / 1_000.0
}

fn main() -> Result<(), Box<dyn Error>> {
    let iterations = parse_count("CACHED_RANGE_ITERATIONS", 10_000)?;
    let warmups = parse_count("CACHED_RANGE_WARMUPS", 1_000)?;
    let fixture = Fixture::create()?;

    let historical = read_range(&fixture.blob, Algorithm::HistoricalInclusive)?;
    let corrected = read_range(&fixture.blob, Algorithm::CorrectedExclusive)?;
    if historical != b"bcd" || corrected != b"bc" {
        return Err("fixture did not expose the historical extra byte".into());
    }

    for iteration in 0..warmups {
        let order = if iteration % 2 == 0 {
            [
                Algorithm::HistoricalInclusive,
                Algorithm::CorrectedExclusive,
            ]
        } else {
            [
                Algorithm::CorrectedExclusive,
                Algorithm::HistoricalInclusive,
            ]
        };
        for algorithm in order {
            black_box(read_range(&fixture.blob, algorithm)?);
        }
    }

    let mut historical = Samples::default();
    let mut corrected = Samples::default();
    for iteration in 0..iterations {
        if iteration % 2 == 0 {
            record(
                &fixture.blob,
                Algorithm::HistoricalInclusive,
                &mut historical,
            )?;
            record(&fixture.blob, Algorithm::CorrectedExclusive, &mut corrected)?;
        } else {
            record(&fixture.blob, Algorithm::CorrectedExclusive, &mut corrected)?;
            record(
                &fixture.blob,
                Algorithm::HistoricalInclusive,
                &mut historical,
            )?;
        }
    }

    let historical_p50 = percentile_us(&mut historical.elapsed_ns, 50);
    let historical_p95 = percentile_us(&mut historical.elapsed_ns, 95);
    let corrected_p50 = percentile_us(&mut corrected.elapsed_ns, 50);
    let corrected_p95 = percentile_us(&mut corrected.elapsed_ns, 95);
    let p50_delta = (corrected_p50 / historical_p50 - 1.0) * 100.0;
    let p95_delta = (corrected_p95 / historical_p95 - 1.0) * 100.0;

    println!(
        concat!(
            "{{\n",
            "  \"benchmark\": \"cached_range_exclusive_ab\",\n",
            "  \"environment\": {{ \"os\": \"{}\", \"arch\": \"{}\" }},\n",
            "  \"range\": {{ \"start\": {}, \"end_exclusive\": {} }},\n",
            "  \"warmups_per_variant\": {},\n",
            "  \"iterations_per_variant\": {},\n",
            "  \"historical_inclusive\": {{ \"bytes\": {}, \"bytes_per_read\": {}, \"p50_us\": {:.3}, \"p95_us\": {:.3}, \"checksum\": {} }},\n",
            "  \"corrected_exclusive\": {{ \"bytes\": {}, \"bytes_per_read\": {}, \"p50_us\": {:.3}, \"p95_us\": {:.3}, \"checksum\": {} }},\n",
            "  \"delta_percent\": {{ \"bytes\": -33.333, \"p50_latency\": {:.3}, \"p95_latency\": {:.3} }}\n",
            "}}"
        ),
        env::consts::OS,
        env::consts::ARCH,
        START,
        END_EXCLUSIVE,
        warmups,
        iterations,
        historical.bytes,
        historical.bytes / iterations as u64,
        historical_p50,
        historical_p95,
        historical.checksum,
        corrected.bytes,
        corrected.bytes / iterations as u64,
        corrected_p50,
        corrected_p95,
        corrected.checksum,
        p50_delta,
        p95_delta,
    );

    Ok(())
}
