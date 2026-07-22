\set ON_ERROR_STOP on
\pset pager off
\pset format unaligned
\pset tuples_only on

-- A/B the post-migration integrity sampler. `ORDER BY random()` assigns and
-- sorts a random float for every blob. BLAKE3/SHA-style hex hashes are already
-- uniformly distributed, so a cryptographically random pivot plus an indexed
-- ordered window yields a rotating sample in O(log N + sample) work.
BEGIN;
CREATE TEMP TABLE perf_verify_blobs (
    hash varchar(64) PRIMARY KEY,
    size bigint NOT NULL
);
INSERT INTO perf_verify_blobs
SELECT md5(n::text) || md5((n + 1000003)::text), 262144
FROM generate_series(1, 1000000) n;
ANALYZE perf_verify_blobs;

-- Exact sample-size/equivalence gates for a middle pivot and wraparound pivot.
SELECT 'middle_count|' || COUNT(*) FROM (
    SELECT hash, size FROM perf_verify_blobs
    WHERE hash >= '8000000000000000000000000000000000000000000000000000000000000000'
    ORDER BY hash LIMIT 100
) sample;
WITH tail AS (
    SELECT hash, size FROM perf_verify_blobs
    WHERE hash >= 'fffff000000000000000000000000000000000000000000000000000000000000'
    ORDER BY hash LIMIT 100
), wrapped AS (
    SELECT * FROM tail
    UNION ALL
    (SELECT hash, size FROM perf_verify_blobs
     WHERE hash < 'fffff000000000000000000000000000000000000000000000000000000000000'
     ORDER BY hash
     LIMIT (100 - (SELECT COUNT(*) FROM tail)))
)
SELECT 'wrap_count|' || COUNT(*) FROM wrapped;

\o /dev/null
\timing on
\echo random_warmup
SELECT hash, size FROM perf_verify_blobs ORDER BY random() LIMIT 100;
\echo indexed_warmup
SELECT hash, size FROM perf_verify_blobs
WHERE hash >= '8000000000000000000000000000000000000000000000000000000000000000'
ORDER BY hash LIMIT 100;

\echo random_1
SELECT hash, size FROM perf_verify_blobs ORDER BY random() LIMIT 100;
\echo indexed_1
SELECT hash, size FROM perf_verify_blobs
WHERE hash >= '8000000000000000000000000000000000000000000000000000000000000000'
ORDER BY hash LIMIT 100;
\echo indexed_2
SELECT hash, size FROM perf_verify_blobs
WHERE hash >= '4000000000000000000000000000000000000000000000000000000000000000'
ORDER BY hash LIMIT 100;
\echo random_2
SELECT hash, size FROM perf_verify_blobs ORDER BY random() LIMIT 100;
\echo random_3
SELECT hash, size FROM perf_verify_blobs ORDER BY random() LIMIT 100;
\echo indexed_3
SELECT hash, size FROM perf_verify_blobs
WHERE hash >= 'c000000000000000000000000000000000000000000000000000000000000000'
ORDER BY hash LIMIT 100;
\echo indexed_4
SELECT hash, size FROM perf_verify_blobs
WHERE hash >= '2000000000000000000000000000000000000000000000000000000000000000'
ORDER BY hash LIMIT 100;
\echo random_4
SELECT hash, size FROM perf_verify_blobs ORDER BY random() LIMIT 100;
\echo random_5
SELECT hash, size FROM perf_verify_blobs ORDER BY random() LIMIT 100;
\echo indexed_5
SELECT hash, size FROM perf_verify_blobs
WHERE hash >= 'e000000000000000000000000000000000000000000000000000000000000000'
ORDER BY hash LIMIT 100;
\timing off
\o
ROLLBACK;
