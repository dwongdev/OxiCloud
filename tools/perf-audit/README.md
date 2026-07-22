# OxiCloud performance audit

This directory is the reproducible evidence log for the 2026-07-21--22
performance audit. It deliberately lives outside every `benches/` directory; the audit did
not use or inspect those directories. Production changes were accepted only
after an A/B gate preserved observable behaviour. Rejected candidates remain
here as evidence, but their production changes were rolled back.

Result labels have six meanings:

- `accepted`: the measured candidate passed correctness and resource gates.
- `rejected`: the candidate regressed a gate or weakened semantics; production
  is unchanged.
- `pending_gate`: evidence is incomplete or a measured resource regression has
  not been explicitly authorized; this is not an acceptance decision.
- `pending_representative_gate`: the benchmark population may not represent the
  production cost distribution closely enough to authorize a tradeoff.
- `pending_explicit_user_tradeoff`: the representative gate is complete, but a
  measured resource regression still needs explicit authorization or rollback.
- `accepted_by_explicit_user_tradeoff`: not Pareto-superior, but the user
  explicitly chose the documented resource tradeoff after seeing both costs.

Heap/RSS figures from in-process Node runs are indicative. Where memory decided
the result, a fresh-process gate was used. SQL harnesses use temporary tables or
disposable databases. Raw samples and environment metadata are retained under
`results/`.

## Decision summary

| Area | Decision | Key measured evidence |
| --- | --- | --- |
| Admin user summary projection | Accepted | Minimal full component path 1.296 -> 0.966 ms, JSON -34.35%, RSS -112 KiB; heavy JSON -99.946%, RSS -132.20 MiB |
| Admin newest-first index | Explicit tradeoff accepted | Unique timestamps: first page 57.217 -> 0.215 ms (266.13x), offset 50k 126.377 -> 5.219 ms (24.21x); 11,255,808 B index; +0.680 us/insert |
| Admin compound newest-first index | Rejected | Unique first page 0.186 -> 0.198 ms; index bytes +80.13%-327.76%; burst inserts +0.445 us/user versus the narrow index |
| Admin `COUNT(*) OVER()` fusion | Rejected | 5.604 -> 42.287 ms (7.55x slower) |
| Folder-upload progress accumulator | Accepted | 1-file repeat 0.797 -> 0.289 us (2.76x); 100-file/5k-update case 6,256.84 -> 13.33 us (469.48x) |
| Delta-worker upload queue cursor | Explicit tradeoff | 1.18x-246.78x faster; median max RSS +112 to +480 KiB; producer-ahead retained RSS +1,008 KiB |
| Frontend whole-file dedup above 10k | Rejected | All-miss 11,384.656 -> 12,545.523 ms with more heap/RSS; production reverted |
| Local blob sync preparation | Accepted | Empty call 25.607 -> 22.946 ns (1.116x; 10/11 process wins); path grouping 1.22x-2.39x; directory preparation 1.23x-212.32x |
| Cached bounded-range length | Accepted | Removed exactly one surplus byte/read; 10k A/B p50 unchanged at 8.459 us, p95 28.292 -> 26.917 us (-4.86%) |
| Manifest-GC hybrid aggregation | Explicit tradeoff accepted | 500 manifests 60.03x with +720 KiB RSS; 1,000 manifests 51.16x with +1,008 KiB RSS; N=0/1 keeps the serial path |
| Integrity verification sorted windows | Explicit tradeoff accepted | Real-FS full method 1.086x-5.766x and remote full method 5.691x-39.056x faster; fresh-process RSS +112 KiB phase 1 and +80 KiB full method |
| Migration work-set paging | Rejected | 65,536-row pages cut RSS 82.74% but were 2.52x slower; 262,144-row pages cut RSS 42.64% but were 2.99x slower |
| Indexed migration verification window | Rejected | Query 267.13x faster, but a 1% contiguous failure range was detected about 1% vs 63.4% for 100 independent samples |
| Loose-chunk DB prefilter | Rejected | Added work on the normal negotiated-miss path and removed backend-missing self-healing semantics |
| Identical-overwrite refcount CTE | Rejected | Mixed legacy/CDC ownership is ambiguous; candidate could undercount live data or leak the shadowed representation |

The original tied-timestamp table also evaluated a compound `(created_at, id)`
index, but posting-list compression invalidated its resource comparison. The
isolated representative A/B/C rerun below supersedes that exploratory result:
the compound candidate failed the no-regression gate and is absent from
production.

## Frontend upload algorithms

`frontend-upload-algorithms.mjs` isolates three algorithms: queue drain,
aggregate progress, and the proposed >10k whole-file dedup batching. It
alternates A/B order, accumulates tiny cases above timer resolution, forces GC
when available, and validates checksums/progress/protocol counts.

Run the general harness from the repository root:

    node --expose-gc tools/perf-audit/frontend-upload-algorithms.mjs \
      --warmup 3 \
      --samples 15 \
      --queue-counts 64,256,1024,10000,50000 \
      --progress-cases 1:100,10:500,100:5000,1000:10000,10000:5000 \
      --hash-counts 1000,10000,10001,25000 \
      --output /tmp/oxicloud-frontend-upload.json

The accepted progress implementation maintains the aggregate sum with
`new_fraction - old_fraction`; restart-to-zero and finalization are covered by
the frontend unit test. The preliminary common run improved every median but
had a noisy one-file p95 regression (2.136 -> 11.240 ms per 1,000-run block,
equivalent to a 2.136 -> 11.240 us/run block average, not a per-event p95), so
it is evidence-only. The
subsequent 41-sample focused gate improved both the normalized one-file median
(0.797 -> 0.289 us/run) and block p95 (14.158 -> 6.338 ms per 1,000 runs,
equivalent to a 14.158 -> 6.338 us/run block average) with identical output.
`progress-common-node26-macos-arm64.json` and
`progress-one-file-repeat-node26-macos-arm64.json` retain both sets of samples.

### Delta-worker queue memory gate

The in-process queue microbenchmark favored the cursor but was biased because
`Array.shift()` ran long enough for V8 to collect while the cursor finished
before the next GC. `queue-memory-gate.mjs` therefore runs every sample in a
fresh process, keeps the permanent ordered chunk table alive, and measures
wall time, max RSS, post-GC retained RSS, and heap for prefilled,
producer-ahead, and balanced shapes.

    node --expose-gc tools/perf-audit/queue-memory-gate.mjs \
      --count 100000 \
      --samples 5 \
      --output tools/perf-audit/results/queue-memory-process-node26-macos-arm64.json

Production uses `cursor-clear-4096`. Against `shift()`, medians were:

| Shape | Wall speedup | Median max RSS delta | Retained RSS delta | Retained heap delta |
| --- | ---: | ---: | ---: | ---: |
| Prefilled | 246.783x | +448 KiB | +448 KiB | -3,080 B |
| Producer ahead | 37.137x | +480 KiB | +1,008 KiB | -3,576 B |
| Balanced | 1.184x | +112 KiB | 0 | +3,168 B |

This, the representative admin index, the manifest-GC hybrid, and the sorted
integrity windows are the audit's explicitly accepted non-Pareto changes. The
queue result JSON marks it
`accepted_by_explicit_user_tradeoff` and retains the rejected thresholds,
`splice`, `slice`, no-clear, and array-reset variants.

### Rejected whole-file dedup batching

The microbenchmark correctly showed that one >10k request is rejected while
bounded requests recover owned hashes. That is functional evidence, not an
acceptance result: the rejected control returns no hashes and does less work.
The decisive loopback workflow includes every dedup, by-hash, and content
request at production upload concurrency:

    node --expose-gc tools/perf-audit/frontend-dedup-workflow.mjs \
      --samples 3 \
      --bytes-per-file 4096 \
      --output tools/perf-audit/results/frontend-dedup-workflow-node26-macos-arm64.json

At 10,001 all-miss files, batching was 10.2% slower and increased median peak
heap/RSS. At 50% hits it saved 50.005% of content bytes and was 1.051x faster,
but roughly doubled peak heap and added about 27.9 MiB RSS. Backend SQL for the
two accepted ownership queries is not modeled, making the candidate optimistic.
The feature was rejected and reverted. The four `dedup-*` JSON files remain
labelled evidence-only so their invalid-control speedups cannot be mistaken for
production acceptance.

## Admin user listing

The accepted compact response fetches only fields rendered by the table;
full-detail API clients keep the previous shape unless `summary=true` is sent.
It avoids detoasting/transporting avatars and preferences. The service-layer
system-admin gate is authoritative, and deterministic pagination uses
`ORDER BY created_at DESC, id DESC`.

Run the projection, count-fusion rejection, representative three-way index
gate, and full component-path gate:

    psql "$DATABASE_URL" -f tools/perf-audit/admin_user_projection.sql
    psql "$DATABASE_URL" -f tools/perf-audit/admin_user_count.sql
    psql "$DATABASE_URL" -f \
      tools/perf-audit/admin_user_order_index_representative.sql
    cargo build --release --manifest-path tools/perf-audit/Cargo.toml \
      --bin admin_user_listing_e2e
    DATABASE_URL="$DATABASE_URL" \
      tools/perf-audit/target/release/admin_user_listing_e2e timing minimal 31
    DATABASE_URL="$DATABASE_URL" \
      tools/perf-audit/target/release/admin_user_listing_e2e timing heavy 11
    /usr/bin/time -l env DATABASE_URL="$DATABASE_URL" \
      tools/perf-audit/target/release/admin_user_listing_e2e memory-historical minimal
    /usr/bin/time -l env DATABASE_URL="$DATABASE_URL" \
      tools/perf-audit/target/release/admin_user_listing_e2e memory-candidate minimal
    /usr/bin/time -l env DATABASE_URL="$DATABASE_URL" \
      tools/perf-audit/target/release/admin_user_listing_e2e memory-historical heavy
    /usr/bin/time -l env DATABASE_URL="$DATABASE_URL" \
      tools/perf-audit/target/release/admin_user_listing_e2e memory-candidate heavy

Repeat each `memory-*` command in three fresh processes; the result file retains
all twelve max-RSS/elapsed samples rather than only the medians.

The projection fixture has 100 users with a 512 KiB avatar and 8 KiB preference
bag each. Its 117.93x timing is the `psql` query/row-transfer/client-decode
gate, not an end-to-end HTTP claim; it excludes Serde and the service-layer
authorization check. The first index fixture has 500,000 users with 100-way
timestamp ties and checks exact order equivalence, first/deep pages, index
bytes, and 10,000-row insert cost. It is not an acceptance result: the
ties deliberately stress incremental sorting, but they also let PostgreSQL
compress the one-column B-tree into posting lists. Its 3.45 MB size and +0.596
us per inserted user understated a normal mostly-unique registration workload.

The decisive component harness includes the full historical SQL/DTO/Serde path
and the candidate's hot Moka flags lookup, system-admin policy check, compact
SQL/DTO, count query, and Serde. It excludes common router/JWT/socket work and
deliberately omits the old handler's intermediate `serde_json::Value`
materialization, making the historical baseline optimistic. It is therefore a
conservative component-path gate rather than a whole HTTP-stack claim. On the
minimal profile, 31 interleaved samples improved median latency 1.296 -> 0.966
ms (1.341x), JSON fell 43,759 -> 28,726 bytes (-34.35%), and three fresh
processes saved 114,688 bytes (112 KiB) median max RSS. With 512 KiB avatars and
8 KiB preferences, median latency improved 1,141.422 -> 0.966 ms, JSON fell
53,306,743 -> 28,726 bytes (-99.946%), and median max RSS fell by 138,625,024
bytes (132.20 MiB). Exact rendered fields, ordering, and counts matched.

The follow-up ran three independent rollback-only transactions, for 15 A/B
samples per shape, with the UUID primary-key index present on both sides. At
500,000 unique timestamps the index was 11,255,808 bytes (3.263x the prior
disclosure), first-page/deep-page reads improved 266.13x/24.21x, and 10,000-row
insert medians imply +0.680 us per user. Ten-user bursts used 4,751,360 bytes,
improved reads 321.57x/18.29x, and added +0.582 us per user. Every initial/final
order and row-count check passed in all three transactions. Because the
representative unique-key disk cost is materially larger than the original
disclosure, the first authorization was invalidated. After seeing the corrected
11,255,808-byte/+0.680-us unique cost and the 4,751,360-byte/+0.582-us burst10
cost, the user explicitly reauthorized the timestamp-only index. It is therefore
`accepted_by_explicit_user_tradeoff`.

The later isolated A/B/C gate retained 15 samples per shape. Versus that
accepted narrow index, the compound index regressed the common unique-timestamp
first page 0.186 -> 0.198 ms (+6.45%), enlarged the unique index 11,255,808 ->
20,275,200 bytes (+80.13%), and enlarged the ten-user-burst index 4,751,360 ->
20,324,352 bytes (+327.76%). It did accelerate deep pages 1.63x-2.30x, but burst
insert medians regressed 44.475 -> 48.921 ms per 10,000 rows (+0.445 us/user).
It therefore failed the no-regression gate and was rejected. Raw A/B and A/B/C
samples are in `admin-user-index-representative-postgres18-macos-arm64.json`;
component-path samples are in
`admin-user-listing-e2e-postgres18-macos-arm64.json`. Production keeps one
narrow online `CREATE INDEX CONCURRENTLY` statement.

## Local blob durability preparation

`local_sync_grouping.rs` A/Bs only the CPU/allocation preparation around the
unchanged fsync work: moving owned `PathBuf`s into task groups instead of
cloning, and a fixed exact-case prefix bitmap instead of sort/dedup of parent
paths. It checks ordered file-path equivalence plus case-sensitive `af`/`aF`
directory equivalence before timing.

    rustc --edition 2024 -O tools/perf-audit/local_sync_grouping.rs -o /tmp/local-sync-grouping
    /tmp/local-sync-grouping

The zero-path gate caught avoidable candidate setup and led to a production
fast return. In 11 fresh processes, each running 31 alternating samples of
100,000 repetitions, it won 10/11 times; the median of process medians improved
25.607 -> 22.946 ns (1.116x). All measured non-empty sizes from 1 to 100,000
paths also improved. The bitmap is fixed at 22x22 slots so uppercase and
lowercase directory names remain distinct on case-sensitive filesystems. Raw
process medians are in `backend-audit-macos-arm64.json`.

## Cached blob bounded ranges

`CachedBlobBackend` was the only backend treating `end` as inclusive even
though the port, Local, S3, Azure, encrypted, CDC, RAM-cache, and HTTP adapter
paths all use `[start, end)`. It consequently read one surplus byte on every
bounded cold-after-fill or hot-cache range. The production fix changes only the
two cached-file limits to `end.saturating_sub(start)` and adds a cold/hot
regression test, including the empty `[3,3)` range.

Run the focused semantic test and standalone hot-file A/B:

    cargo test --lib \
      cached_blob_backend::tests::range_end_is_exclusive_on_cold_and_hot_cache_reads
    cargo run --release --manifest-path tools/perf-audit/Cargo.toml \
      --bin cached_range_ab

For 10,000 interleaved `[1,3)` reads, the historical path returned 30,000
bytes versus the correct 20,000 (-33.333% for this two-byte fixture). Median
latency stayed 8.459 us; p95 improved from 28.292 to 26.917 us (-4.86%). The
cold fill remained exactly one origin GET/six bytes. The fixed Local/cached
length vectors both equal `[1,2,0,4]`; the historical cached vector was
`[2,3,1,4]`. Exact evidence is in
`cached_range_exclusive_2026-07-22.json`.

## Manifest garbage collection

`gc_manifest_batch.rs` compares the historical serial update per deleted
manifest with several measured candidates. Production keeps the accepted
hybrid: the dominant empty sweep uses the original simple `DELETE RETURNING`,
one returned manifest uses the original serial update, and batches of two or
more aggregate exact distinct-per-manifest decrements in an owned `HashMap`
before one `UPDATE FROM unnest`.

The crossover was positive at two manifests. At 500, statements fell 502 -> 3
and median latency 1,456.096 -> 24.255 ms (60.03x); at 1,000, 1,003 -> 5 and
3,163.190 -> 61.829 ms (51.16x). Five fresh processes measured no RSS change at
two, +720 KiB at 500, and +1,008 KiB at 1,000. The user explicitly accepted
that bounded memory tradeoff, so the result is
`accepted_by_explicit_user_tradeoff`.

The atomic all-in-one CTE was rejected because an all-live sweep regressed
15.59%-44.84%. Borrowed SQLx binds saved memory but regressed large-batch
latency; sorted/RLE scratch was not Pareto either. All variants validate live
controls, shared and repeated chunks, exact refcounts, underflow,
`orphaned_at`, and exact statement counts.

Reproduce the threshold, large-batch, and fresh-process resource gates:

    OXICLOUD_POSTGRES_HOST=192.168.107.2 \
      GC_SCENARIOS='0:500,1:499,2:498,4:496,8:492,32:468' \
      GC_HYBRID_THRESHOLDS='2,4,8,32,500' GC_WARMUPS=2 GC_SAMPLES=9 \
      bash tools/perf-audit/run_gc_manifest_batch.sh

    OXICLOUD_POSTGRES_HOST=192.168.107.2 \
      GC_SCENARIOS='500:10,1000:10' GC_HYBRID_THRESHOLDS=2 \
      GC_WARMUPS=1 GC_SAMPLES=5 \
      bash tools/perf-audit/run_gc_manifest_batch.sh

    OXICLOUD_POSTGRES_HOST=192.168.107.2 GC_RSS_RUNS=5 \
      bash tools/perf-audit/run_gc_manifest_bind_rss.sh

The scripts create randomly named disposable databases and drop them on
success, failure, or interruption. Exact samples and rejected candidates are
in `gc_manifest_batch_2026-07-21.json`.

## Integrity verification

`verify_integrity_borrowed.rs` compares the historical serial backend-size
probe per manifest occurrence with owned, borrowed-hash-map, and sorted
borrowed-key candidates. The accepted implementation keeps the exact serial
path through four valid occurrences. Above that gate it processes bounded
256-occurrence windows, sorts and deduplicates borrowed `&str` keys, probes at
concurrency 8 with `FuturesUnordered`, and replays issue generation in original
manifest/occurrence order. Malformed manifests retain their historical
no-probe behaviour.

    OXICLOUD_AUDIT_CONCURRENCY=8 \
    cargo run --release --manifest-path tools/perf-audit/Cargo.toml \
      --bin verify_integrity_borrowed -- --real-fs

    OXICLOUD_AUDIT_CONCURRENCY=8 \
    cargo run --release --manifest-path tools/perf-audit/Cargo.toml \
      --bin verify_integrity_borrowed -- --remote-only

The first unbounded table doubled max RSS and was rejected. A concurrent path
for two/four immediate probes was 62x-67x slower and was also rejected. The
intermediate owned-key window at concurrency 16 added 176 KiB RSS and was
superseded. The final sorted/borrowed concurrency-8 scheduler was tested with
the same boxed-future shape used by production. Across 31-sample real-filesystem
gates it improved the full method 1.086x for unique hashes, 1.095x for a mixed
existing/missing set, and 5.766x for shared hashes. The remote full-method gates
improved 5.691x for unique and 39.056x for shared hashes. Backend calls never
increased, issue order was exact, malformed manifests performed zero probes,
and the `1x2`, `2x1`, and `1x4` cases execute the same serial code.

Eleven fresh-process runs over 250,000 unique occurrences measured the accepted
candidate at +112 KiB (+0.4284%) RSS for phase 1 and +80 KiB (+0.3053%) for the
full method. After disclosure of a measured peak cost up to 112 KiB, the user
explicitly reauthorized retaining the candidate in exchange for the measured
speedup. Build once, then reproduce the RSS modes separately so the compiler is
not part of the measurement. Run each timed command in 11 fresh processes and
compare medians:

    cargo build --release --manifest-path tools/perf-audit/Cargo.toml \
      --bin verify_integrity_borrowed
    OXICLOUD_AUDIT_CONCURRENCY=8 /usr/bin/time -l \
      tools/perf-audit/target/release/verify_integrity_borrowed \
      --memory historical phase
    OXICLOUD_AUDIT_CONCURRENCY=8 /usr/bin/time -l \
      tools/perf-audit/target/release/verify_integrity_borrowed \
      --memory sorted phase
    OXICLOUD_AUDIT_CONCURRENCY=8 /usr/bin/time -l \
      tools/perf-audit/target/release/verify_integrity_borrowed \
      --memory historical full
    OXICLOUD_AUDIT_CONCURRENCY=8 /usr/bin/time -l \
      tools/perf-audit/target/release/verify_integrity_borrowed \
      --memory sorted full

`verify_integrity_streaming.rs` additionally tested direct SQLx streaming and a
bounded producer/channel with 16 prefetched manifest rows against a disposable
PostgreSQL database. The producer/channel candidate cut RSS 74.23% and made
phase 1 1.722x faster, but its same-round full-method median regressed 4.17%, so
it was rejected and no streaming code entered production. Reproduce both SQLx
experiments with:

    bash tools/perf-audit/run_verify_integrity_streaming.sh
    bash tools/perf-audit/run_verify_integrity_prefetch.sh

The accepted measurements and raw gates are in
`verify_integrity_sorted_c8_2026-07-22.json`; the rejected SQLx result is in
`verify_integrity_streaming_2026-07-22.json`. The earlier owned-window evidence
is retained in `verify_integrity_phase1_2026-07-21.json` as a rejected,
superseded candidate.

## Rejected migration work-set paging

`migration_workset.rs` compares the current one-million-row ordered work-set
materialization with bounded keyset pages. Every mode ran in a fresh client
process and had to return exactly 1,000,000 rows in the same order/checksum.

Seed and run against a disposable PostgreSQL database:

    cargo run --release --manifest-path tools/perf-audit/Cargo.toml \
      --bin migration_workset -- seed 1000000
    cargo run --release --manifest-path tools/perf-audit/Cargo.toml \
      --bin migration_workset -- current
    cargo run --release --manifest-path tools/perf-audit/Cargo.toml \
      --bin migration_workset -- paged 65536
    cargo run --release --manifest-path tools/perf-audit/Cargo.toml \
      --bin migration_workset -- paged 262144

The 65,536-row page reduced median process RSS from 106,053,632 to 18,300,928
bytes (-82.74%) but increased median query/consume time from 644.398 to
1,622.317 ms (+151.76%, 2.52x). The 262,144-row page used 60,833,792 bytes
(-42.64%) and took 1,928.731 ms (+199.31%, 2.99x). Both candidates therefore
failed the no-latency-regression gate and production remains unchanged. The
container bridge was noisy; only complete three-way rounds were retained, and
every transport failure is listed in
`migration-workset-postgres18-macos-arm64.json`.

## Rejected migration verification sampler

`migration_verify_sampling.sql` measures replacing `ORDER BY random()` with a
random pivot followed by one contiguous indexed hash window:

    psql "$DATABASE_URL" -f tools/perf-audit/migration_verify_sampling.sql

The query improved from 100.442 to 0.376 ms on one million rows (267.13x), but
the samples are correlated. For a 1% contiguous/prefix failure range, one
100-row successor window detects the failure about 1% of the time; 100
independent samples detect it with probability `1 - 0.99^100 = 63.4%`. The
semantic regression rejected the candidate and production was reverted. See
`migration-verify-sampling-postgres18-macos-arm64.json`.

## Rejected storage candidates

`rejected_storage_candidates_2026-07-21.json` records two fully rolled-back
experiments. Their Rust files are archived diagnostic source snapshots rather
than registered binaries in the standalone perf Cargo package.

### Loose-chunk prefilter

`rejected_delta_loose_hit_probe.rs` counted physical object-store PUTs/bytes for
400 x 256 KiB frames. The browser protocol already negotiates missing hashes,
so all-miss is the normal receive path. A DB prefilter would add queries and up
to 8 MiB request buffering there. More importantly, a metadata row does not
prove the backend object exists: skipping PUT based only on PostgreSQL would
remove the current self-healing overwrite for missing objects. No candidate
showed a Pareto win across miss latency, RAM, remote bytes, and repair semantics.

### Identical-overwrite refcount CTE

`rejected_refcount_overwrite_probe.rs` exercised the public write port on
legacy, CDC-manifest, different-hash, delete/GC, missing-file, SQL-error, and
lifecycle-hook fixtures. The proposed CTE fixed unambiguous same-representation
cases, but `storage.files` stores only a hash. When legacy `storage.blobs` and a
new `storage.chunk_manifests` row coexist under that hash, the swap cannot know
which representation owns the displaced reference. It can decrement live CDC
state or preserve a shadowed legacy reference/bytes. Timing samples also had
enough container jitter that no non-regression claim was possible. The CTE was
rejected and fully reverted.

The result also exposes a pre-existing baseline issue: repeated identical
legacy overwrites increased refcount from 1 to 1,001 in the 1,000-iteration
fixture. It remains unfixed because the attempted shortcut could turn a leak
into undercount/data loss. A future fix needs explicit representation ownership
or normalization before another benchmarked candidate is safe.

## Video thumbnail diagnostic utility

`video-thumbnail-server.mjs` is a browser-side real-media gate for a possible
thumbnail fallback change. It can serve failed thumbnail responses, a
range-capable WebM original, and thumbnail PUT sinks. No production decision in
this audit depends on it.

Generate a deterministic fixture:

    ffmpeg -y -hide_banner -loglevel error -f lavfi \
      -i testsrc2=size=640x360:rate=30 -t 8 -c:v libvpx-vp9 \
      -b:v 2M -deadline realtime -cpu-used 8 -an \
      /tmp/oxicloud-thumbnail-perf.webm

Any future candidate using this gate must run in fresh browser contexts,
alternate A/B order, and reject a supposedly no-download path if it emits any
original-video GET or thumbnail PUT.
