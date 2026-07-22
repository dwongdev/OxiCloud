#!/usr/bin/env node

// Process-isolated memory gate for the delta worker's upload queue.
//
// The earlier in-process heap delta was biased: Array.shift() runs long enough
// for V8 to collect garbage during the measurement, while the cursor finishes
// before the next GC. This harness gives every sample a fresh Node process and
// compares max RSS plus post-GC retained RSS/heap. It models the worker's
// permanent ordered `chunks` array as well as the second uploadQueue reference.

import { spawnSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import process from 'node:process';
import { performance } from 'node:perf_hooks';

const UPLOAD_BATCH_BYTES = 8 * 1024 * 1024;
const MODES = [
  { name: 'current-shift', cursor: false, clear: false, threshold: 0, compact: 'none' },
  {
    name: 'cursor-clear-1024',
    cursor: true,
    clear: true,
    threshold: 1024,
    compact: 'copy',
  },
  {
    name: 'cursor-clear-4096',
    cursor: true,
    clear: true,
    threshold: 4096,
    compact: 'copy',
  },
  {
    name: 'cursor-clear-16384',
    cursor: true,
    clear: true,
    threshold: 16384,
    compact: 'copy',
  },
  {
    name: 'cursor-no-clear-4096',
    cursor: true,
    clear: false,
    threshold: 4096,
    compact: 'copy',
  },
  {
    name: 'cursor-splice-4096',
    cursor: true,
    clear: true,
    threshold: 4096,
    compact: 'splice',
  },
  {
    name: 'cursor-splice-16384',
    cursor: true,
    clear: true,
    threshold: 16384,
    compact: 'splice',
  },
  {
    name: 'cursor-slice-4096',
    cursor: true,
    clear: true,
    threshold: 4096,
    compact: 'slice',
  },
  {
    name: 'cursor-reset-4096',
    cursor: true,
    clear: true,
    threshold: 4096,
    compact: 'copy',
    resetWhenEmpty: true,
  },
];
const SHAPES = ['prefilled', 'streaming-ahead', 'streaming-balanced'];

function parseArgs(argv) {
  const out = new Map();
  for (let index = 0; index < argv.length; index += 2) {
    out.set(argv[index], argv[index + 1]);
  }
  return out;
}

function positiveInteger(name, raw) {
  const value = Number(raw);
  if (!Number.isInteger(value) || value < 1) {
    throw new Error(`${name} must be an integer >= 1; received ${raw}`);
  }
  return value;
}

function chunkAt(index) {
  const size = (1 + ((Math.imul(index + 1, 2_654_435_761) >>> 28) & 7)) * 32 * 1024;
  return { h: `chunk-${index}`, s: size, offset: index * 32 * 1024 };
}

function fold(checksum, chunk) {
  return Math.imul(checksum ^ chunk.s ^ (chunk.offset >>> 0), 16_777_619) >>> 0;
}

function runChild(mode, shape, count) {
  if (typeof globalThis.gc !== 'function') {
    throw new Error('child must run with --expose-gc');
  }

  const chunks = Array.from({ length: count }, (_, index) => chunkAt(index));
  let queue = shape === 'prefilled' ? chunks.slice() : [];
  let head = 0;
  let checksum = 2_166_136_261;
  let consumed = 0;
  let batches = 0;
  let compactions = 0;

  const available = () => (mode.cursor ? queue.length - head : queue.length);
  const drainBatch = () => {
    if (available() === 0) return false;
    let bytes = 0;
    while (available() > 0 && bytes < UPLOAD_BATCH_BYTES) {
      let chunk;
      if (mode.cursor) {
        chunk = queue[head];
        if (mode.clear) queue[head] = undefined;
        head++;
      } else {
        chunk = queue.shift();
      }
      checksum = fold(checksum, chunk);
      bytes += chunk.s;
      consumed++;
    }

    if (mode.cursor) {
      if (head === queue.length) {
        if (mode.resetWhenEmpty) queue = [];
        else queue.length = 0;
        head = 0;
      } else if (head >= mode.threshold && head * 2 >= queue.length) {
        if (mode.compact === 'splice') {
          queue.splice(0, head);
        } else if (mode.compact === 'slice') {
          queue = queue.slice(head);
        } else {
          queue.copyWithin(0, head);
          queue.length -= head;
        }
        head = 0;
        compactions++;
      }
    }
    batches++;
    return true;
  };

  globalThis.gc();
  const baseline = process.memoryUsage();
  const started = performance.now();

  if (shape === 'prefilled') {
    while (drainBatch()) {}
  } else {
    const drainsPerProduce = shape === 'streaming-ahead' ? 1 : 5;
    const produceBatch = 256;
    for (let start = 0; start < chunks.length; start += produceBatch) {
      queue.push(...chunks.slice(start, Math.min(start + produceBatch, chunks.length)));
      for (let drain = 0; drain < drainsPerProduce; drain++) {
        if (!drainBatch()) break;
      }
    }
    while (drainBatch()) {}
  }

  const wallMs = performance.now() - started;
  const maxRssBytes = process.resourceUsage().maxRSS * 1024;
  globalThis.gc();
  const after = process.memoryUsage();

  // Keep the production-equivalent ordered chunk table live through the final
  // measurement. The queue must be logically empty in every implementation.
  checksum ^= chunks.length;
  if (consumed !== count || available() !== 0) {
    throw new Error(`queue invariant failed: consumed=${consumed}, available=${available()}`);
  }

  return {
    mode: mode.name,
    shape,
    count,
    wallMs,
    checksum: checksum >>> 0,
    consumed,
    batches,
    compactions,
    baselineRssBytes: baseline.rss,
    baselineHeapBytes: baseline.heapUsed,
    maxRssBytes,
    peakRssDeltaBytes: Math.max(0, maxRssBytes - baseline.rss),
    retainedRssDeltaBytes: after.rss - baseline.rss,
    retainedHeapDeltaBytes: after.heapUsed - baseline.heapUsed,
  };
}

function median(values) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.floor(sorted.length / 2)];
}

const args = parseArgs(process.argv.slice(2));
if (args.has('--child')) {
  const modeName = args.get('--mode');
  const mode = MODES.find((candidate) => candidate.name === modeName);
  if (!mode) throw new Error(`unknown mode ${modeName}`);
  const shape = args.get('--shape');
  if (!SHAPES.includes(shape)) throw new Error(`unknown shape ${shape}`);
  const count = positiveInteger('count', args.get('--count'));
  process.stdout.write(`${JSON.stringify(runChild(mode, shape, count))}\n`);
  process.exit(0);
}

const count = positiveInteger('count', args.get('--count') ?? '100000');
const samples = positiveInteger('samples', args.get('--samples') ?? '5');
const output = args.get('--output');
const rows = [];

for (let sample = 0; sample < samples; sample++) {
  const modes = sample % 2 === 0 ? MODES : [...MODES].reverse();
  const shapes = sample % 2 === 0 ? SHAPES : [...SHAPES].reverse();
  for (const shape of shapes) {
    for (const mode of modes) {
      const child = spawnSync(
        process.execPath,
        [
          '--expose-gc',
          new URL(import.meta.url).pathname,
          '--child',
          '1',
          '--mode',
          mode.name,
          '--shape',
          shape,
          '--count',
          String(count),
        ],
        { encoding: 'utf8', maxBuffer: 1024 * 1024 },
      );
      if (child.status !== 0) {
        throw new Error(`child failed (${mode.name}/${shape}): ${child.stderr || child.stdout}`);
      }
      rows.push(JSON.parse(child.stdout.trim()));
    }
  }
}

for (const shape of SHAPES) {
  const reference = rows.find((row) => row.shape === shape && row.mode === MODES[0].name);
  for (const row of rows.filter((candidate) => candidate.shape === shape)) {
    if (
      row.checksum !== reference.checksum ||
      row.consumed !== reference.consumed ||
      row.batches !== reference.batches
    ) {
      throw new Error(`semantic mismatch for ${shape}/${row.mode}`);
    }
  }
}

const results = SHAPES.flatMap((shape) =>
  MODES.map((mode) => {
    const samplesForMode = rows.filter((row) => row.shape === shape && row.mode === mode.name);
    return {
      shape,
      mode: mode.name,
      wallMedianMs: Number(median(samplesForMode.map((row) => row.wallMs)).toFixed(3)),
      maxRssBytesMedian: median(samplesForMode.map((row) => row.maxRssBytes)),
      peakRssDeltaBytesMedian: median(samplesForMode.map((row) => row.peakRssDeltaBytes)),
      retainedRssDeltaBytesMedian: median(samplesForMode.map((row) => row.retainedRssDeltaBytes)),
      retainedHeapDeltaBytesMedian: median(samplesForMode.map((row) => row.retainedHeapDeltaBytes)),
      maxRssSamplesBytes: samplesForMode.map((row) => row.maxRssBytes),
      peakRssDeltaSamplesBytes: samplesForMode.map((row) => row.peakRssDeltaBytes),
      compactions: samplesForMode[0].compactions,
    };
  }),
);

const rendered = `${JSON.stringify(
  {
    schemaVersion: 1,
    generatedAt: new Date().toISOString(),
    environment: { node: process.version, platform: process.platform, arch: process.arch },
    fixture: { chunks: count, samples, uploadBatchBytes: UPLOAD_BATCH_BYTES },
    note: 'Each row is a fresh process; maxRSS is process.resourceUsage().maxRSS. The ordered chunks table remains live through final GC.',
    results,
  },
  null,
  2,
)}\n`;
if (output) writeFileSync(output, rendered);
process.stdout.write(rendered);
