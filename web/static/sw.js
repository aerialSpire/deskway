// The client's shell, kept for when the network is not there: the page, its scripts, styles,
// texts and icons. Always from the network first, so a new version shows on the next load; the
// kept copy only when the network fails, and then the page says it cannot reach the server.
// Nothing of the API or the signaling socket is kept.
const CACHE = 'deskway-shell';

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));

self.addEventListener('fetch', (e) => {
  const url = new URL(e.request.url);
  if (e.request.method !== 'GET' || url.origin !== location.origin || url.pathname.startsWith('/api/') || url.pathname.startsWith('/ws/')) return;
  e.respondWith((async () => {
    const cache = await caches.open(CACHE);
    try {
      const res = await fetch(e.request);
      if (res.ok) await cache.put(e.request, res.clone());
      return res;
    } catch (err) {
      const kept = await cache.match(e.request, { ignoreSearch: true }) || (e.request.mode === 'navigate' ? await cache.match('./') : null);
      if (kept) return kept;
      throw err;
    }
  })());
});
