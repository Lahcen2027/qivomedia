// Qivomedia player service worker: keeps the player running with no network.
const SHELL = "qivo-shell-v2";
const MEDIA = "qivo-media";
const API = "qivo-api";
const SHELL_URLS = ["/player/", "/player/player.js", "/shared/schedule.js", "/player/manifest.webmanifest"];

self.addEventListener("install", (e) => {
  e.waitUntil(caches.open(SHELL).then((c) => c.addAll(SHELL_URLS)).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (e) => {
  e.waitUntil(self.clients.claim());
});

async function rangeFromCache(req, cached) {
  const range = req.headers.get("range");
  if (!range) return cached;
  const buf = await cached.arrayBuffer();
  const m = /bytes=(\d*)-(\d*)/.exec(range);
  const size = buf.byteLength;
  let start = m && m[1] ? parseInt(m[1], 10) : 0;
  let end = m && m[2] ? parseInt(m[2], 10) : size - 1;
  if (m && !m[1] && m[2]) { start = size - parseInt(m[2], 10); end = size - 1; }
  end = Math.min(end, size - 1);
  return new Response(buf.slice(start, end + 1), {
    status: 206,
    headers: {
      "Content-Type": cached.headers.get("Content-Type") || "application/octet-stream",
      "Content-Range": `bytes ${start}-${end}/${size}`,
      "Content-Length": String(end - start + 1),
      "Accept-Ranges": "bytes",
    },
  });
}

self.addEventListener("fetch", (e) => {
  const url = new URL(e.request.url);
  if (e.request.method !== "GET" || url.origin !== location.origin) return;

  // Media: cache-first (content-addressed, immutable).
  if (url.pathname.startsWith("/files/")) {
    e.respondWith((async () => {
      const cache = await caches.open(MEDIA);
      const hit = await cache.match(url.pathname, { ignoreVary: true });
      if (hit) return rangeFromCache(e.request, hit);
      return fetch(e.request);
    })());
    return;
  }

  // Manifest: network-first, fall back to last good copy.
  if (url.pathname === "/api/player/manifest") {
    e.respondWith((async () => {
      const cache = await caches.open(API);
      try {
        const res = await fetch(e.request);
        if (res.ok) cache.put("/api/player/manifest", res.clone());
        return res;
      } catch {
        const hit = await cache.match("/api/player/manifest", { ignoreVary: true });
        return hit || new Response(JSON.stringify({ status: "offline" }), { status: 503, headers: { "Content-Type": "application/json" } });
      }
    })());
    return;
  }

  // App shell: network-first, cache fallback.
  if (url.pathname.startsWith("/player") || url.pathname.startsWith("/shared/")) {
    e.respondWith((async () => {
      const cache = await caches.open(SHELL);
      try {
        const res = await fetch(e.request);
        if (res.ok) cache.put(url.pathname === "/player" ? "/player/" : e.request, res.clone());
        return res;
      } catch {
        return (await cache.match(url.pathname, { ignoreVary: true })) || (await cache.match("/player/", { ignoreVary: true }));
      }
    })());
  }
});
