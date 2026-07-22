#!/usr/bin/env node

import { createServer } from "node:http";
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { parseArgs } from "node:util";
import os from "node:os";

const MAX_DEDUP_HASHES = 10_000;
const UPLOAD_BATCH_BYTES = 8 * 1024 * 1024;
const CURSOR_COMPACT_AT = 4_096;

let blackhole = 0;

const { values } = parseArgs({
  options: {
    suite: { type: "string", default: "all" },
    warmup: { type: "string", default: "3" },
    samples: { type: "string", default: "15" },
    "queue-counts": { type: "string", default: "64,256,1024,10000,50000" },
    "progress-cases": {
      type: "string",
      default: "1:100,10:500,100:5000,1000:10000,10000:5000",
    },
    "hash-counts": { type: "string", default: "1000,10000,10001,25000" },
    "dedup-batch-size": { type: "string", default: "10000" },
    "dedup-concurrency": { type: "string", default: "4" },
    "server-latency-ms": { type: "string", default: "0" },
    "modeled-file-bytes": { type: "string", default: "65536" },
    output: { type: "string" },
  },
  strict: true,
  allowPositionals: false,
});

function positiveInteger(name, raw, allowZero = false) {
  const value = Number(raw);
  const lowerBound = allowZero ? 0 : 1;
  if (!Number.isInteger(value) || value < lowerBound) {
    throw new Error(
      name + " must be an integer >= " + lowerBound + "; received " + raw,
    );
  }
  return value;
}

function numberList(name, raw) {
  const parsed = raw
    .split(",")
    .filter(Boolean)
    .map((part) => positiveInteger(name, part));
  if (parsed.length === 0) throw new Error(name + " must not be empty");
  return parsed;
}

function progressCases(raw) {
  const parsed = raw
    .split(",")
    .filter(Boolean)
    .map((entry) => {
      const parts = entry.split(":");
      if (parts.length !== 2)
        throw new Error("Invalid progress case: " + entry);
      return {
        items: positiveInteger("progress items", parts[0]),
        updates: positiveInteger("progress updates", parts[1]),
      };
    });
  if (parsed.length === 0) throw new Error("progress-cases must not be empty");
  return parsed;
}

const config = {
  suite: values.suite,
  warmup: positiveInteger("warmup", values.warmup, true),
  samples: positiveInteger("samples", values.samples),
  queueCounts: numberList("queue-counts", values["queue-counts"]),
  progressCases: progressCases(values["progress-cases"]),
  hashCounts: numberList("hash-counts", values["hash-counts"]),
  dedupBatchSize: positiveInteger(
    "dedup-batch-size",
    values["dedup-batch-size"],
  ),
  dedupConcurrency: positiveInteger(
    "dedup-concurrency",
    values["dedup-concurrency"],
  ),
  serverLatencyMs: positiveInteger(
    "server-latency-ms",
    values["server-latency-ms"],
    true,
  ),
  modeledFileBytes: positiveInteger(
    "modeled-file-bytes",
    values["modeled-file-bytes"],
  ),
};

if (!["all", "queue", "progress", "dedup"].includes(config.suite)) {
  throw new Error("suite must be all, queue, progress, or dedup");
}
if (config.dedupBatchSize > MAX_DEDUP_HASHES) {
  throw new Error(
    "dedup-batch-size must be <= the server limit of " + MAX_DEDUP_HASHES,
  );
}

function median(sorted) {
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 0
    ? (sorted[middle - 1] + sorted[middle]) / 2
    : sorted[middle];
}

function summarize(samples) {
  const times = samples.map((sample) => sample.ms).sort((a, b) => a - b);
  const heaps = samples
    .map((sample) => sample.heapDeltaBytes)
    .sort((a, b) => a - b);
  const rss = samples
    .map((sample) => sample.rssDeltaBytes)
    .sort((a, b) => a - b);
  const p95Index = Math.max(0, Math.ceil(times.length * 0.95) - 1);
  return {
    sampleCount: samples.length,
    medianMs: median(times),
    p95Ms: times[p95Index],
    minMs: times[0],
    maxMs: times[times.length - 1],
    medianHeapDeltaBytes: median(heaps),
    medianRssDeltaBytes: median(rss),
  };
}

function consume(result) {
  const token = Number(
    result.checksum ??
      result.ownedCount ??
      result.chunkCount ??
      result.lastPercent ??
      0,
  );
  blackhole = (blackhole ^ (token >>> 0)) >>> 0;
}

async function measureOne(fn) {
  if (global.gc) global.gc();
  const before = process.memoryUsage();
  const started = performance.now();
  const result = await fn();
  const ms = performance.now() - started;
  const after = process.memoryUsage();
  consume(result);
  return {
    sample: {
      ms,
      heapDeltaBytes: after.heapUsed - before.heapUsed,
      rssDeltaBytes: after.rss - before.rss,
    },
    result,
  };
}

async function benchmarkPair(currentFn, candidateFn, verify) {
  const checkedCurrent = await currentFn();
  const checkedCandidate = await candidateFn();
  verify(checkedCurrent, checkedCandidate);

  for (let i = 0; i < config.warmup; i++) {
    if (i % 2 === 0) {
      consume(await currentFn());
      consume(await candidateFn());
    } else {
      consume(await candidateFn());
      consume(await currentFn());
    }
  }

  const currentSamples = [];
  const candidateSamples = [];
  let currentResult = checkedCurrent;
  let candidateResult = checkedCandidate;
  for (let i = 0; i < config.samples; i++) {
    const order =
      i % 2 === 0
        ? [
            ["current", currentFn],
            ["candidate", candidateFn],
          ]
        : [
            ["candidate", candidateFn],
            ["current", currentFn],
          ];
    for (const [kind, fn] of order) {
      const measured = await measureOne(fn);
      if (kind === "current") {
        currentSamples.push(measured.sample);
        currentResult = measured.result;
      } else {
        candidateSamples.push(measured.sample);
        candidateResult = measured.result;
      }
    }
  }

  const current = summarize(currentSamples);
  const candidate = summarize(candidateSamples);
  return {
    current,
    candidate,
    speedup: current.medianMs / candidate.medianMs,
    representative: {
      current: currentResult,
      candidate: candidateResult,
    },
  };
}

function makeChunks(count) {
  const chunks = new Array(count);
  let offset = 0;
  for (let i = 0; i < count; i++) {
    const size =
      (1 + ((Math.imul(i + 1, 2_654_435_761) >>> 28) & 7)) * 32 * 1024;
    chunks[i] = { h: "chunk-" + i, s: size, offset };
    offset += size;
  }
  return chunks;
}

function foldBatch(batch, checksum) {
  let next = checksum;
  for (const chunk of batch) {
    next = Math.imul(next ^ chunk.s ^ (chunk.offset >>> 0), 16_777_619) >>> 0;
  }
  return next;
}

function drainWithShift(source) {
  const uploadQueue = source.slice();
  let checksum = 2_166_136_261;
  let chunkCount = 0;
  let totalBytes = 0;
  let batchCount = 0;

  while (uploadQueue.length > 0) {
    const batch = [];
    let bytes = 0;
    while (uploadQueue.length > 0 && bytes < UPLOAD_BATCH_BYTES) {
      const chunk = uploadQueue.shift();
      batch.push(chunk);
      bytes += chunk.s;
    }
    checksum = foldBatch(batch, checksum);
    chunkCount += batch.length;
    totalBytes += bytes;
    batchCount++;
  }

  return { checksum, chunkCount, totalBytes, batchCount };
}

function drainWithCursor(source) {
  const uploadQueue = source.slice();
  let head = 0;
  let checksum = 2_166_136_261;
  let chunkCount = 0;
  let totalBytes = 0;
  let batchCount = 0;
  let compactions = 0;

  while (head < uploadQueue.length) {
    const batch = [];
    let bytes = 0;
    while (head < uploadQueue.length && bytes < UPLOAD_BATCH_BYTES) {
      const chunk = uploadQueue[head];
      uploadQueue[head] = undefined;
      head++;
      batch.push(chunk);
      bytes += chunk.s;
    }
    checksum = foldBatch(batch, checksum);
    chunkCount += batch.length;
    totalBytes += bytes;
    batchCount++;

    if (head === uploadQueue.length) {
      uploadQueue.length = 0;
      head = 0;
    } else if (head >= CURSOR_COMPACT_AT && head * 2 >= uploadQueue.length) {
      uploadQueue.copyWithin(0, head);
      uploadQueue.length -= head;
      head = 0;
      compactions++;
    }
  }

  return { checksum, chunkCount, totalBytes, batchCount, compactions };
}

function verifyQueue(current, candidate) {
  for (const key of ["checksum", "chunkCount", "totalBytes", "batchCount"]) {
    if (current[key] !== candidate[key]) {
      throw new Error(
        "Queue candidate changed " +
          key +
          ": " +
          current[key] +
          " vs " +
          candidate[key],
      );
    }
  }
}

function makeProgressEvents(items, updateCount) {
  const indices = new Uint32Array(updateCount);
  const values = new Float64Array(updateCount);
  let state = 0x9e3779b9;
  for (let i = 0; i < updateCount; i++) {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    state >>>= 0;
    indices[i] = state % items;
    values[i] = (state & 1023) / 1024;
  }
  return { indices, values };
}

function recordProgress(checksum, sum, total) {
  const percent = Math.round((sum / total) * 100);
  const done = Math.round(sum);
  return {
    checksum: (checksum + Math.imul(percent + 1, done + 1)) >>> 0,
    percent,
  };
}

function progressWithFullScan(items, events) {
  const fractions = new Array(items).fill(0);
  let checksum = 0;
  let lastPercent = 0;
  let finalSum = 0;
  for (let update = 0; update < events.indices.length; update++) {
    fractions[events.indices[update]] = Math.min(1, events.values[update]);
    let sum = 0;
    for (const fraction of fractions) sum += fraction;
    const recorded = recordProgress(checksum, sum, items);
    checksum = recorded.checksum;
    lastPercent = recorded.percent;
    finalSum = sum;
  }
  return { checksum, lastPercent, finalSum };
}

function progressWithAccumulator(items, events) {
  const fractions = new Array(items).fill(0);
  let sum = 0;
  let checksum = 0;
  let lastPercent = 0;
  for (let update = 0; update < events.indices.length; update++) {
    const index = events.indices[update];
    const next = Math.min(1, events.values[update]);
    sum += next - fractions[index];
    fractions[index] = next;
    const recorded = recordProgress(checksum, sum, items);
    checksum = recorded.checksum;
    lastPercent = recorded.percent;
  }
  return { checksum, lastPercent, finalSum: sum };
}

function verifyProgress(current, candidate) {
  if (
    current.checksum !== candidate.checksum ||
    current.lastPercent !== candidate.lastPercent
  ) {
    throw new Error("Progress candidate changed user-visible progress values");
  }
  if (Math.abs(current.finalSum - candidate.finalSum) > Number.EPSILON * 8) {
    throw new Error("Progress candidate changed final sum");
  }
}

function makeHashes(count) {
  const hashes = new Array(count);
  for (let i = 0; i < count; i++) hashes[i] = i.toString(16).padStart(64, "0");
  return hashes;
}

function isOwnedHash(hash) {
  return (Number.parseInt(hash.at(-1), 16) & 1) === 0;
}

function emptyServerStats() {
  return {
    requests: 0,
    acceptedRequests: 0,
    rejectedRequests: 0,
    requestBytes: 0,
    responseBytes: 0,
    maxBatchHashes: 0,
  };
}

async function startDedupServer() {
  let activeStats = emptyServerStats();
  const server = createServer(async (request, response) => {
    const parts = [];
    for await (const part of request) parts.push(part);
    const body = Buffer.concat(parts);
    const parsed = JSON.parse(body.toString("utf8"));
    const hashes = Array.isArray(parsed.hashes) ? parsed.hashes : [];

    activeStats.requests++;
    activeStats.requestBytes += body.byteLength;
    activeStats.maxBatchHashes = Math.max(
      activeStats.maxBatchHashes,
      hashes.length,
    );

    if (config.serverLatencyMs > 0) {
      await new Promise((resolveDelay) =>
        setTimeout(resolveDelay, config.serverLatencyMs),
      );
    }

    let status;
    let responseBody;
    if (hashes.length > MAX_DEDUP_HASHES) {
      status = 400;
      activeStats.rejectedRequests++;
      responseBody = JSON.stringify({ error: "Too many hashes" });
    } else {
      status = 200;
      activeStats.acceptedRequests++;
      responseBody = JSON.stringify({ owned: hashes.filter(isOwnedHash) });
    }
    activeStats.responseBytes += Buffer.byteLength(responseBody);
    response.writeHead(status, { "content-type": "application/json" });
    response.end(responseBody);
  });

  await new Promise((resolveListen, rejectListen) => {
    server.once("error", rejectListen);
    server.listen(0, "127.0.0.1", resolveListen);
  });
  const address = server.address();
  if (!address || typeof address === "string")
    throw new Error("Could not determine mock server address");

  return {
    url: "http://127.0.0.1:" + address.port + "/api/dedup/check-batch",
    resetStats() {
      activeStats = emptyServerStats();
      return activeStats;
    },
    async close() {
      await new Promise((resolveClose, rejectClose) => {
        server.close((error) => (error ? rejectClose(error) : resolveClose()));
      });
    },
  };
}

async function postHashes(url, hashes) {
  const response = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ hashes }),
  });
  if (!response.ok) return new Set();
  const data = await response.json().catch(() => null);
  return new Set(data?.owned ?? []);
}

async function dedupCurrent(url, hashes) {
  return postHashes(url, hashes);
}

async function dedupBatched(url, hashes) {
  // Production keeps the former one-request path exact for the overwhelmingly
  // common valid case: no slice, batching array, or Promise pool below the cap.
  if (hashes.length <= MAX_DEDUP_HASHES) return postHashes(url, hashes);

  const batches = [];
  for (let start = 0; start < hashes.length; start += config.dedupBatchSize) {
    batches.push(hashes.slice(start, start + config.dedupBatchSize));
  }

  const owned = new Set();
  let next = 0;
  const worker = async () => {
    while (next < batches.length) {
      const index = next++;
      const batchOwned = await postHashes(url, batches[index]);
      for (const hash of batchOwned) owned.add(hash);
    }
  };
  await Promise.all(
    Array.from(
      { length: Math.min(config.dedupConcurrency, batches.length) },
      worker,
    ),
  );
  return owned;
}

function dedupRun(server, hashes, implementation) {
  return async () => {
    const stats = server.resetStats();
    const owned = await implementation(server.url, hashes);
    let checksum = 0;
    for (const hash of owned)
      checksum = (checksum + Number.parseInt(hash.slice(-8), 16)) >>> 0;
    return {
      checksum,
      ownedCount: owned.size,
      contentBytesAvoided: owned.size * config.modeledFileBytes,
      ...stats,
    };
  };
}

function verifyDedup(current, candidate, hashCount) {
  const expectedOwned = Math.ceil(hashCount / 2);
  if (hashCount <= MAX_DEDUP_HASHES) {
    if (
      current.ownedCount !== expectedOwned ||
      candidate.ownedCount !== expectedOwned ||
      current.checksum !== candidate.checksum
    ) {
      throw new Error("Dedup fast path changed the ownership result");
    }
    if (
      current.requests !== 1 ||
      candidate.requests !== 1 ||
      current.rejectedRequests !== 0 ||
      candidate.rejectedRequests !== 0
    ) {
      throw new Error("Dedup fast path must remain one accepted request");
    }
    return;
  }

  if (current.ownedCount !== 0 || current.rejectedRequests !== 1) {
    throw new Error(
      "Current >10k control did not reproduce the expected rejection",
    );
  }
  if (candidate.ownedCount !== expectedOwned) {
    throw new Error(
      "Batched candidate found " +
        candidate.ownedCount +
        " owned hashes; expected " +
        expectedOwned,
    );
  }
  if (
    candidate.rejectedRequests !== 0 ||
    candidate.maxBatchHashes > MAX_DEDUP_HASHES
  ) {
    throw new Error("Batched candidate exceeded the server request limit");
  }
}

function repeatQueueDrain(fn, repetitions) {
  let checksum = 0;
  let chunkCount = 0;
  let totalBytes = 0;
  let batchCount = 0;
  for (let iteration = 0; iteration < repetitions; iteration++) {
    const result = fn();
    checksum = (checksum + result.checksum) >>> 0;
    chunkCount += result.chunkCount;
    totalBytes += result.totalBytes;
    batchCount += result.batchCount;
  }
  return { checksum, chunkCount, totalBytes, batchCount };
}

function repeatProgress(fn, repetitions) {
  let checksum = 0;
  let lastPercent = 0;
  let finalSum = 0;
  for (let iteration = 0; iteration < repetitions; iteration++) {
    const result = fn();
    checksum = (checksum + result.checksum) >>> 0;
    lastPercent = result.lastPercent;
    finalSum += result.finalSum;
  }
  return { checksum, lastPercent, finalSum };
}

function formatMs(value) {
  if (value >= 100) return value.toFixed(1);
  if (value >= 10) return value.toFixed(2);
  return value.toFixed(3);
}

function formatBytes(value) {
  const absolute = Math.abs(value);
  const sign = value < 0 ? "-" : "";
  if (absolute >= 1024 * 1024 * 1024)
    return sign + (absolute / (1024 * 1024 * 1024)).toFixed(2) + " GiB";
  if (absolute >= 1024 * 1024)
    return sign + (absolute / (1024 * 1024)).toFixed(2) + " MiB";
  if (absolute >= 1024) return sign + (absolute / 1024).toFixed(2) + " KiB";
  return sign + absolute.toFixed(0) + " B";
}

function printPair(label, result) {
  console.log(label);
  console.log(
    "  current   median " +
      formatMs(result.current.medianMs) +
      " ms; p95 " +
      formatMs(result.current.p95Ms) +
      " ms; heap delta " +
      formatBytes(result.current.medianHeapDeltaBytes),
  );
  console.log(
    "  candidate median " +
      formatMs(result.candidate.medianMs) +
      " ms; p95 " +
      formatMs(result.candidate.p95Ms) +
      " ms; heap delta " +
      formatBytes(result.candidate.medianHeapDeltaBytes),
  );
  console.log("  median speedup " + result.speedup.toFixed(2) + "x");
}

async function runQueueSuite(output) {
  output.queue = [];
  for (const count of config.queueCounts) {
    const source = makeChunks(count);
    const repetitions = count <= 1024 ? Math.ceil(200_000 / count) : 1;
    const result = await benchmarkPair(
      () => repeatQueueDrain(() => drainWithShift(source), repetitions),
      () => repeatQueueDrain(() => drainWithCursor(source), repetitions),
      verifyQueue,
    );
    output.queue.push({
      chunkCount: count,
      repetitions,
      normalizedMedianUsPerDrain: {
        current: (result.current.medianMs * 1000) / repetitions,
        candidate: (result.candidate.medianMs * 1000) / repetitions,
      },
      ...result,
    });
    printPair(
      "A queue drain, " +
        count.toLocaleString("en-US") +
        " chunks x " +
        repetitions.toLocaleString("en-US"),
      result,
    );
  }
}

async function runProgressSuite(output) {
  output.progress = [];
  for (const scenario of config.progressCases) {
    const events = makeProgressEvents(scenario.items, scenario.updates);
    const repetitions =
      scenario.items <= 100 ? Math.ceil(100_000 / scenario.updates) : 1;
    const result = await benchmarkPair(
      () =>
        repeatProgress(
          () => progressWithFullScan(scenario.items, events),
          repetitions,
        ),
      () =>
        repeatProgress(
          () => progressWithAccumulator(scenario.items, events),
          repetitions,
        ),
      verifyProgress,
    );
    output.progress.push({
      ...scenario,
      repetitions,
      normalizedMedianUsPerRun: {
        current: (result.current.medianMs * 1000) / repetitions,
        candidate: (result.candidate.medianMs * 1000) / repetitions,
      },
      ...result,
    });
    printPair(
      "B aggregate progress, " +
        scenario.items.toLocaleString("en-US") +
        " files x " +
        scenario.updates.toLocaleString("en-US") +
        " updates",
      result,
    );
  }
}

async function runDedupSuite(output) {
  output.dedup = [];
  const server = await startDedupServer();
  try {
    for (const hashCount of config.hashCounts) {
      const hashes = makeHashes(hashCount);
      const result = await benchmarkPair(
        dedupRun(server, hashes, dedupCurrent),
        dedupRun(server, hashes, dedupBatched),
        (current, candidate) => verifyDedup(current, candidate, hashCount),
      );
      output.dedup.push({ hashCount, ...result });
      printPair(
        "C dedup HTTP probe, " + hashCount.toLocaleString("en-US") + " hashes",
        result,
      );
      if (hashCount <= MAX_DEDUP_HASHES) {
        console.log(
          "  both paths used one accepted request and found " +
            result.representative.current.ownedCount +
            " owned hashes",
        );
      } else {
        console.log(
          "  current rejected " +
            result.representative.current.rejectedRequests +
            " request and found " +
            result.representative.current.ownedCount +
            " owned hashes",
        );
      }
      console.log(
        "  candidate used " +
          result.representative.candidate.requests +
          " accepted batches, found " +
          result.representative.candidate.ownedCount +
          ", and avoided " +
          formatBytes(result.representative.candidate.contentBytesAvoided) +
          " of modeled content upload",
      );
    }
  } finally {
    await server.close();
  }
}

const output = {
  schemaVersion: 1,
  generatedAt: new Date().toISOString(),
  environment: {
    node: process.version,
    platform: process.platform,
    release: os.release(),
    arch: process.arch,
    cpu: os.cpus()[0]?.model ?? "unknown",
    logicalCpus: os.cpus().length,
    gcExposed: Boolean(global.gc),
  },
  config,
  notes: {
    heap: "Median heap delta is indicative only; timing is the primary microbenchmark metric.",
    dedup:
      "The current >10k path is faster only because it is rejected and returns no owned hashes.",
  },
  suites: {},
};

if (!global.gc) {
  console.warn("Warning: run with --expose-gc for less noisy heap deltas.");
}
console.log(
  "Node " +
    process.version +
    "; warmup " +
    config.warmup +
    "; samples " +
    config.samples +
    "; GC exposed " +
    Boolean(global.gc),
);

if (config.suite === "all" || config.suite === "queue")
  await runQueueSuite(output.suites);
if (config.suite === "all" || config.suite === "progress")
  await runProgressSuite(output.suites);
if (config.suite === "all" || config.suite === "dedup")
  await runDedupSuite(output.suites);

output.blackhole = blackhole;

if (values.output) {
  const destination = resolve(values.output);
  await mkdir(dirname(destination), { recursive: true });
  await writeFile(destination, JSON.stringify(output, null, 2) + "\n");
  console.log("Wrote JSON result to " + destination);
}
