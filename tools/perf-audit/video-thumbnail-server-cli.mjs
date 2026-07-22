#!/usr/bin/env node

import { startVideoThumbnailServer } from "./video-thumbnail-server.mjs";

const fixturePath = process.argv[2] ?? "/tmp/oxicloud-thumbnail-perf.webm";
const server = await startVideoThumbnailServer({ fixturePath });

console.log(
  JSON.stringify({ url: server.url, fixtureBytes: server.fixtureBytes }),
);

let closing = false;
async function close() {
  if (closing) return;
  closing = true;
  await server.close();
  process.exit(0);
}

process.on("SIGINT", () => void close());
process.on("SIGTERM", () => void close());
await new Promise(() => {});
