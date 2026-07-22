#!/usr/bin/env node

// End-to-end loopback gate for the >10k whole-file dedup batching change.
// Unlike the probe-only microbenchmark, this executes every subsequent
// by-hash or content-upload request with the production upload concurrency.

import { createServer } from 'node:http';
import { writeFileSync } from 'node:fs';
import { performance } from 'node:perf_hooks';
import process from 'node:process';

const MAX_HASHES = 10_000;
const HASH_COUNT = 10_001;
const BATCH_CONCURRENCY = 4;
const UPLOAD_CONCURRENCY = 2;

const args = new Map();
for (let index = 2; index < process.argv.length; index += 2) {
  args.set(process.argv[index], process.argv[index + 1]);
}
const samples = Number(args.get('--samples') ?? 3);
const bytesPerFile = Number(args.get('--bytes-per-file') ?? 4096);
const output = args.get('--output');
if (!Number.isInteger(samples) || samples < 1) throw new Error('samples must be >= 1');
if (!Number.isInteger(bytesPerFile) || bytesPerFile < 1) {
  throw new Error('bytes-per-file must be >= 1');
}

const hashes = Array.from({ length: HASH_COUNT }, (_, index) =>
  index.toString(16).padStart(64, '0'),
);
const content = Buffer.alloc(bytesPerFile, 0x5a);

function emptyStats(hitPercent) {
  return {
    hitPercent,
    dedupAccepted: 0,
    dedupRejected: 0,
    dedupRequestBytes: 0,
    dedupResponseBytes: 0,
    uploadRequests: 0,
    uploadContentBytes: 0,
    byHashRequests: 0,
    byHashRequestBytes: 0,
  };
}

let stats = emptyStats(0);
function owned(hash) {
  return stats.hitPercent === 50 && (Number.parseInt(hash.at(-1), 16) & 1) === 0;
}

function send(response, status, body) {
  response.writeHead(status, { 'content-type': 'application/json' });
  response.end(body);
}

const server = createServer(async (request, response) => {
  const chunks = [];
  for await (const chunk of request) chunks.push(chunk);
  const body = Buffer.concat(chunks);

  if (request.url === '/api/dedup/check-batch') {
    stats.dedupRequestBytes += body.byteLength;
    const parsed = JSON.parse(body.toString('utf8'));
    const requestHashes = Array.isArray(parsed.hashes) ? parsed.hashes : [];
    if (requestHashes.length > MAX_HASHES) {
      stats.dedupRejected++;
      const responseBody = JSON.stringify({ error: 'Too many hashes' });
      stats.dedupResponseBytes += Buffer.byteLength(responseBody);
      send(response, 400, responseBody);
      return;
    }
    stats.dedupAccepted++;
    const responseBody = JSON.stringify({ owned: requestHashes.filter(owned) });
    stats.dedupResponseBytes += Buffer.byteLength(responseBody);
    send(response, 200, responseBody);
    return;
  }

  if (request.url === '/api/files/by-hash') {
    stats.byHashRequests++;
    stats.byHashRequestBytes += body.byteLength;
    send(response, 201, '{"ok":true}');
    return;
  }

  if (request.url === '/api/files/upload') {
    stats.uploadRequests++;
    stats.uploadContentBytes += body.byteLength;
    send(response, 201, '{"ok":true}');
    return;
  }

  send(response, 404, '{}');
});

await new Promise((resolve, reject) => {
  server.once('error', reject);
  server.listen(0, '127.0.0.1', resolve);
});
const address = server.address();
if (!address || typeof address === 'string') throw new Error('server address unavailable');
const baseUrl = `http://127.0.0.1:${address.port}`;

async function post(path, body, contentType) {
  const response = await fetch(baseUrl + path, {
    method: 'POST',
    headers: { 'content-type': contentType },
    body,
  });
  const text = await response.text();
  return { ok: response.ok, text };
}

async function requestOwned(requestHashes) {
  const response = await post(
    '/api/dedup/check-batch',
    JSON.stringify({ hashes: requestHashes }),
    'application/json',
  );
  if (!response.ok) return null;
  const decoded = JSON.parse(response.text);
  return Array.isArray(decoded.owned) ? decoded.owned : null;
}

async function currentProbe() {
  return new Set((await requestOwned(hashes)) ?? []);
}

async function candidateProbe() {
  const ownedHashes = new Set();
  const waveSize = MAX_HASHES * BATCH_CONCURRENCY;
  for (let waveStart = 0; waveStart < hashes.length; waveStart += waveSize) {
    const requests = [];
    const waveEnd = Math.min(hashes.length, waveStart + waveSize);
    for (let start = waveStart; start < waveEnd; start += MAX_HASHES) {
      requests.push(requestOwned(hashes.slice(start, Math.min(start + MAX_HASHES, waveEnd))));
    }
    const responses = await Promise.all(requests);
    if (responses.some((batch) => batch === null)) return new Set();
    for (const batch of responses) for (const hash of batch) ownedHashes.add(hash);
  }
  return ownedHashes;
}

async function mapIndexes(limit, operation) {
  let next = 0;
  await Promise.all(
    Array.from({ length: limit }, async () => {
      while (next < hashes.length) {
        const index = next++;
        await operation(index);
      }
    }),
  );
}

async function runWorkflow(hitPercent, probe) {
  stats = emptyStats(hitPercent);
  if (globalThis.gc) globalThis.gc();
  const before = process.memoryUsage();
  let peakHeap = before.heapUsed;
  let peakRss = before.rss;
  const sampler = setInterval(() => {
    const memory = process.memoryUsage();
    peakHeap = Math.max(peakHeap, memory.heapUsed);
    peakRss = Math.max(peakRss, memory.rss);
  }, 1);

  const start = performance.now();
  const ownedHashes = await probe();
  await mapIndexes(UPLOAD_CONCURRENCY, async (index) => {
    const hash = hashes[index];
    if (ownedHashes.has(hash)) {
      const response = await post(
        '/api/files/by-hash',
        JSON.stringify({ folder_id: 'folder', name: `file-${index}`, hash }),
        'application/json',
      );
      if (!response.ok) throw new Error('by-hash request failed');
    } else {
      const response = await post('/api/files/upload', content, 'application/octet-stream');
      if (!response.ok) throw new Error('content upload failed');
    }
  });
  const wallMs = performance.now() - start;
  clearInterval(sampler);
  const after = process.memoryUsage();
  peakHeap = Math.max(peakHeap, after.heapUsed);
  peakRss = Math.max(peakRss, after.rss);

  return {
    wallMs,
    ownedCount: ownedHashes.size,
    peakHeapDeltaBytes: Math.max(0, peakHeap - before.heapUsed),
    peakRssDeltaBytes: Math.max(0, peakRss - before.rss),
    ...stats,
  };
}

function median(values) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.floor(sorted.length / 2)];
}

function summarize(runs) {
  return {
    wallSamplesMs: runs.map((run) => Number(run.wallMs.toFixed(3))),
    wallMedianMs: Number(median(runs.map((run) => run.wallMs)).toFixed(3)),
    peakHeapDeltaBytesMedian: median(runs.map((run) => run.peakHeapDeltaBytes)),
    peakRssDeltaBytesMedian: median(runs.map((run) => run.peakRssDeltaBytes)),
    protocol: Object.fromEntries(
      Object.entries(runs[0]).filter(([key]) => !key.includes('Delta') && key !== 'wallMs'),
    ),
  };
}

const cases = [];
try {
  // Warm undici's connection pool and JIT without exercising the measured
  // >10k workflow.
  await post('/api/files/upload', content, 'application/octet-stream');
  await post(
    '/api/files/by-hash',
    JSON.stringify({ folder_id: 'folder', name: 'warm', hash: hashes[0] }),
    'application/json',
  );

  for (const hitPercent of [0, 50]) {
    const currentRuns = [];
    const candidateRuns = [];
    for (let sample = 0; sample < samples; sample++) {
      if (sample % 2 === 0) {
        currentRuns.push(await runWorkflow(hitPercent, currentProbe));
        candidateRuns.push(await runWorkflow(hitPercent, candidateProbe));
      } else {
        candidateRuns.push(await runWorkflow(hitPercent, candidateProbe));
        currentRuns.push(await runWorkflow(hitPercent, currentProbe));
      }
    }

    const expectedOwned = hitPercent === 50 ? Math.ceil(HASH_COUNT / 2) : 0;
    for (const run of candidateRuns) {
      if (run.ownedCount !== expectedOwned || run.dedupRejected !== 0) {
        throw new Error(`candidate correctness failure at ${hitPercent}% hits`);
      }
    }
    for (const run of currentRuns) {
      if (run.ownedCount !== 0 || run.dedupRejected !== 1) {
        throw new Error(`control did not reproduce >10k rejection at ${hitPercent}% hits`);
      }
    }

    const current = summarize(currentRuns);
    const candidate = summarize(candidateRuns);
    cases.push({
      hitPercent,
      current,
      candidate,
      wallSpeedup: Number((current.wallMedianMs / candidate.wallMedianMs).toFixed(3)),
      uploadByteReductionPercent: Number(
        (
          100 *
          (1 -
            candidate.protocol.uploadContentBytes / current.protocol.uploadContentBytes)
        ).toFixed(3),
      ),
    });
  }
} finally {
  await new Promise((resolve, reject) =>
    server.close((error) => (error ? reject(error) : resolve())),
  );
}

const result = {
  schemaVersion: 1,
  generatedAt: new Date().toISOString(),
  environment: { node: process.version, platform: process.platform, arch: process.arch },
  fixture: { hashes: HASH_COUNT, bytesPerFile, uploadConcurrency: UPLOAD_CONCURRENCY },
  note: 'Loopback mock includes every dedup, by-hash and content request. Hashing is excluded. Backend SQL is unmodeled: current rejects before ownership lookup while candidate would execute two accepted queries, so candidate wall time is optimistic.',
  cases,
};
const rendered = JSON.stringify(result, null, 2) + '\n';
if (output) writeFileSync(output, rendered);
process.stdout.write(rendered);
