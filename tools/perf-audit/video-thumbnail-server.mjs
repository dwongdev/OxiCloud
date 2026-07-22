import { readFile } from "node:fs/promises";
import { createServer } from "node:http";

function freshStats() {
  return {
    thumbnailMissRequests: 0,
    originalVideoRequests: 0,
    originalVideoBytes: 0,
    generatedThumbnailPuts: 0,
    generatedThumbnailBytes: 0,
  };
}

function pageHtml() {
  return [
    "<!doctype html>",
    "<html><head><meta charset='utf-8'><title>thumbnail perf</title></head>",
    "<body><main id='root'></main><script>",
    "const params = new URLSearchParams(location.search);",
    "const mode = params.get('mode') === 'candidate' ? 'candidate' : 'current';",
    "const count = Math.max(1, Number(params.get('videos') || 6));",
    "const sizes = [[150, 150, 'icon'], [300, 300, 'preview'], [900, 800, 'large']];",
    "const result = { mode, count, completed: 0, errors: 0, wallMs: 0 };",
    "window.__thumbnailPerf = result;",
    "window.__thumbnailPerfDone = false;",
    "const started = performance.now();",
    "let active = 0;",
    "const waiting = [];",
    "function complete(ok) {",
    "  result.completed++;",
    "  if (!ok) result.errors++;",
    "  if (result.completed === count) {",
    "    result.wallMs = performance.now() - started;",
    "    window.__thumbnailPerfDone = true;",
    "    document.title = 'done';",
    "  }",
    "}",
    "function blobToDataUrl(blob) {",
    "  return new Promise((resolve, reject) => {",
    "    const reader = new FileReader();",
    "    reader.onload = () => resolve(reader.result);",
    "    reader.onerror = reject;",
    "    reader.readAsDataURL(blob);",
    "  });",
    "}",
    "function videoBitmap(id) {",
    "  return new Promise((resolve, reject) => {",
    "    const video = document.createElement('video');",
    "    video.muted = true;",
    "    video.preload = 'metadata';",
    "    video.style.display = 'none';",
    "    document.body.append(video);",
    "    const clean = () => {",
    "      video.pause();",
    "      video.removeAttribute('src');",
    "      video.load();",
    "      video.remove();",
    "    };",
    "    video.onloadedmetadata = () => { video.currentTime = Math.max(0.1, video.duration / 3); };",
    "    video.onseeked = async () => {",
    "      try {",
    "        const bitmap = await createImageBitmap(video);",
    "        clean();",
    "        resolve(bitmap);",
    "      } catch (error) { clean(); reject(error); }",
    "    };",
    "    video.onerror = () => { clean(); reject(new Error('video decode failed')); };",
    "    video.src = '/api/files/video-' + id;",
    "  });",
    "}",
    "async function generate(id) {",
    "  const bitmap = await videoBitmap(id);",
    "  const blobs = await Promise.all(sizes.map(async ([targetWidth, targetHeight]) => {",
    "    const ratio = Math.min(targetWidth / bitmap.width, targetHeight / bitmap.height);",
    "    const canvas = new OffscreenCanvas(Math.round(bitmap.width * ratio), Math.round(bitmap.height * ratio));",
    "    canvas.getContext('2d').drawImage(bitmap, 0, 0, canvas.width, canvas.height);",
    "    return canvas.convertToBlob({ type: 'image/jpeg', quality: 0.8 });",
    "  }));",
    "  bitmap.close();",
    "  await blobToDataUrl(blobs[0]);",
    "  await Promise.all(blobs.map((blob, index) => fetch(",
    "    '/api/files/video-' + id + '/thumbnail/' + sizes[index][2],",
    "    { method: 'PUT', headers: { 'content-type': 'image/jpeg' }, body: blob }",
    "  )));",
    "}",
    "async function onThumbnailError(id, image) {",
    "  image.style.display = 'none';",
    "  if (mode === 'candidate') { complete(true); return; }",
    "  if (active >= 3) await new Promise((resolve) => waiting.push(resolve));",
    "  active++;",
    "  try { await generate(id); complete(true); } catch (error) { complete(false); }",
    "  finally { active--; const next = waiting.shift(); if (next) next(); }",
    "}",
    "for (let id = 0; id < count; id++) {",
    "  const image = document.createElement('img');",
    "  image.alt = '';",
    "  image.onerror = () => { void onThumbnailError(id, image); };",
    "  image.src = '/thumbnail/video-' + id;",
    "  document.getElementById('root').append(image);",
    "}",
    "</script></body></html>",
  ].join("\n");
}

function parseRange(header, size) {
  const match = /^bytes=(\d+)-(\d*)$/.exec(header ?? "");
  if (!match) return null;
  const start = Number(match[1]);
  const end = match[2] ? Math.min(Number(match[2]), size - 1) : size - 1;
  if (!Number.isSafeInteger(start) || start < 0 || start > end || start >= size)
    return null;
  return { start, end };
}

export async function startVideoThumbnailServer({ fixturePath }) {
  const video = await readFile(fixturePath);
  const html = Buffer.from(pageHtml());
  let stats = freshStats();

  const server = createServer(async (request, response) => {
    try {
      const url = new URL(request.url ?? "/", "http://127.0.0.1");
      if (request.method === "GET" && url.pathname === "/") {
        response.writeHead(200, {
          "content-type": "text/html; charset=utf-8",
          "content-length": html.byteLength,
          "cache-control": "no-store",
        });
        response.end(html);
        return;
      }
      if (request.method === "POST" && url.pathname === "/__reset") {
        stats = freshStats();
        response.writeHead(204, { "cache-control": "no-store" });
        response.end();
        return;
      }
      if (request.method === "GET" && url.pathname === "/__stats") {
        const body = Buffer.from(JSON.stringify(stats));
        response.writeHead(200, {
          "content-type": "application/json",
          "content-length": body.byteLength,
          "cache-control": "no-store",
        });
        response.end(body);
        return;
      }
      if (request.method === "GET" && url.pathname.startsWith("/thumbnail/")) {
        stats.thumbnailMissRequests++;
        response.writeHead(204, { "cache-control": "no-store" });
        response.end();
        return;
      }
      if (
        request.method === "GET" &&
        /^\/api\/files\/video-\d+$/.test(url.pathname)
      ) {
        stats.originalVideoRequests++;
        const range = parseRange(request.headers.range, video.byteLength);
        const start = range?.start ?? 0;
        const end = range?.end ?? video.byteLength - 1;
        const body = video.subarray(start, end + 1);
        stats.originalVideoBytes += body.byteLength;
        response.writeHead(range ? 206 : 200, {
          "content-type": "video/webm",
          "content-length": body.byteLength,
          "accept-ranges": "bytes",
          "cache-control": "no-store",
          ...(range
            ? {
                "content-range":
                  "bytes " + start + "-" + end + "/" + video.byteLength,
              }
            : {}),
        });
        response.end(body);
        return;
      }
      if (
        request.method === "PUT" &&
        /^\/api\/files\/video-\d+\/thumbnail\/(icon|preview|large)$/.test(
          url.pathname,
        )
      ) {
        let bytes = 0;
        for await (const chunk of request) bytes += chunk.length;
        stats.generatedThumbnailPuts++;
        stats.generatedThumbnailBytes += bytes;
        response.writeHead(201, { "content-length": "0" });
        response.end();
        return;
      }
      response.writeHead(404, { "content-length": "0" });
      response.end();
    } catch (error) {
      response.writeHead(500, { "content-type": "text/plain" });
      response.end(error instanceof Error ? error.message : String(error));
    }
  });

  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  if (!address || typeof address === "string")
    throw new Error("No loopback server address");

  return {
    url: "http://127.0.0.1:" + address.port + "/",
    fixtureBytes: video.byteLength,
    resetStats() {
      stats = freshStats();
    },
    snapshot() {
      return { ...stats };
    },
    async close() {
      await new Promise((resolve, reject) => {
        server.close((error) => (error ? reject(error) : resolve()));
      });
    },
  };
}
