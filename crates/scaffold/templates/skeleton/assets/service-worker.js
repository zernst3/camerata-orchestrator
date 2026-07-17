// Basic offline-caching service worker for the {{APP_NAME}} PWA. Cache-first for the
// app shell + static assets (stale-while-revalidate: serve from cache immediately,
// refetch in the background to keep the cache warm); network requests to server
// functions and any future /api/* routes always pass through uncached, since those
// carry live data.
const CACHE_NAME = "{{APP_NAME_SNAKE}}-cache-v1";
const PRECACHE_ASSETS = [
  "/",
  "/static/manifest.json",
  "/static/styles/index.css",
  "/static/design/tokens.css",
  "/static/design/components.css",
];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches.open(CACHE_NAME).then((cache) => {
      return cache.addAll(PRECACHE_ASSETS).catch((err) => {
        console.warn("Service worker: failed to precache some assets:", err);
      });
    })
  );
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys().then((keys) => {
      return Promise.all(
        keys.filter((key) => key !== CACHE_NAME).map((key) => caches.delete(key))
      );
    })
  );
  self.clients.claim();
});

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);

  // Never cache server-function calls or API routes — those are always live data.
  if (url.pathname.startsWith("/api/") || event.request.method !== "GET") {
    return;
  }

  event.respondWith(
    caches.match(event.request).then((cached) => {
      const network = fetch(event.request)
        .then((response) => {
          if (response.ok) {
            caches.open(CACHE_NAME).then((cache) => cache.put(event.request, response.clone()));
          }
          return response;
        })
        .catch(() => cached);

      // Stale-while-revalidate: serve the cached copy instantly if we have one,
      // otherwise wait on the network.
      return cached || network;
    })
  );
});
