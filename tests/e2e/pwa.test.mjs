// The client as an app: installable (manifest, icons, the offline shell), the shell never keeping
// the API, the offline page, a new version said on the pages but never in a session, the account
// menu's way to install (the browser's prompt, or Safari's steps), and the Home Screen app's
// first sign-in and device name.
import { sleep, t, q } from './harness.mjs';

export const area = {
  name: 'pwa',
  needs: ['auth'],
  async run(h) {
    const p = await h.newPage();
    await p.until(`!!document.querySelector('.card')`, 'my computers');

    // Installable: the manifest, its icons, a service worker; Chrome's own checks find nothing.
    const manifest = (await p.send('Page.getAppManifest')).result;
    const m = JSON.parse(manifest.data || '{}');
    h.check('the manifest reads without errors: standalone, scoped, with any and maskable icons', !manifest.errors?.length && m.display === 'standalone' && m.icons?.some((i) => i.purpose === 'maskable') && m.icons?.some((i) => i.sizes === '512x512' && i.purpose === 'any'), JSON.stringify({ errors: manifest.errors, display: m.display, icons: m.icons }));
    const icons = await p.eval(`Promise.all(['icons/icon-192.png', 'icons/icon-512.png', 'icons/maskable-512.png', 'icons/apple-touch-icon.png'].map((u) => fetch(u).then((r) => r.status + ' ' + r.headers.get('content-type'))))`);
    h.check('every icon is served as a PNG', icons.every((x) => x === '200 image/png'), JSON.stringify(icons));
    await p.until(`navigator.serviceWorker && navigator.serviceWorker.controller !== undefined && (navigator.serviceWorker.ready, true)`, 'service worker API');
    const sw = await p.eval(`navigator.serviceWorker.ready.then((r) => !!r.active && r.active.scriptURL.endsWith('/sw.js'))`);
    h.check('the shell is kept by a service worker', sw);
    await p.eval(`location.reload()`);
    await p.until(`!!document.querySelector('.card')`, 'my computers, reloaded under the worker');
    const errors = (await p.send('Page.getInstallabilityErrors')).result?.installabilityErrors || [];
    h.check('Chrome finds nothing in the way of installing it', !errors.length, JSON.stringify(errors));
    const kept = await p.eval(`caches.open('deskway-shell').then((c) => c.keys()).then((ks) => ks.map((k) => new URL(k.url).pathname))`);
    h.check('the shell keeps the page and its files, never the API', kept.some((x) => x.endsWith('/style.css')) && !kept.some((x) => x.startsWith('/api/') || x.startsWith('/ws/')), JSON.stringify(kept));

    // A new version: a notice with "Reload" on the pages; never during a session.
    const newer = `d.serverSays({ type: 'hello', version: 3, caps: [], build: 'newer-build' })`;
    await h.sessionOn(p);
    await p.v(newer);
    await sleep(300);
    h.check('a new version during a session changes nothing there', await p.v(`!!s && s.info().connection === 'connected'`) && await p.eval(`!document.querySelector('.page .notice .btn')`));
    await p.v(`s.leave()`);
    await p.until(`!s`, 'session over');
    await p.v(`d.show('list')`);
    await p.until(`!!document.querySelector('.card')`, 'back on my computers');
    h.check('back on the pages, the new version is said, with Reload', await p.eval(`(() => { const n = document.querySelector('.page > .notice'); return !!n && n.textContent.includes(${q('update.ready')}) && !!n.querySelector('.btn'); })()`));
    await p.shot('pwa-update-phone', 390, 844);

    // The browser's install prompt, from the account menu.
    await p.eval(`(() => { const e = new Event('beforeinstallprompt'); e.prompt = async () => { window.__prompted = true; }; dispatchEvent(e); })()`);
    await p.eval(`document.querySelector('.name-btn').click()`);
    const item = await p.eval(`[...document.querySelectorAll('.menu-item')].map((x) => x.textContent)`);
    h.check('where the browser offers it, the account menu says Install app', item.includes(t('menu.install')), JSON.stringify(item));
    await p.shot('pwa-menu-phone', 390, 844);
    await p.eval(`[...document.querySelectorAll('.menu-item')].find((x) => x.textContent === ${q('menu.install')}).click()`);
    await sleep(200);
    h.check('…and opens the browser’s own install prompt', await p.eval(`window.__prompted === true`));

    // Offline: the kept shell says it cannot reach the server.
    await p.send('Network.enable');
    await p.send('Network.emulateNetworkConditions', { offline: true, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
    await p.eval(`location.reload()`);
    await p.until(`document.querySelector('.heading h1')?.textContent === ${q('unreach.title')}`, 'offline page', 15000);
    h.check('offline, the kept shell shows the same page as an unreachable server, with Retry', await p.eval(`!!document.querySelector('.topbar .brand') && [...document.querySelectorAll('.btn')].some((b) => b.textContent === ${q('common.retry')})`));
    await p.shot('pwa-offline-phone', 390, 844);
    await p.send('Network.emulateNetworkConditions', { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
    await h.closePage(p);

    // Safari: navigator.standalone, and no install prompt (this Chrome would offer one), so the
    // menu explains Add to Home Screen.
    const s = await h.newPage();
    await s.send('Page.addScriptToEvaluateOnNewDocument', { source: `Object.defineProperty(Navigator.prototype, 'standalone', { get: () => false, configurable: true }); addEventListener('beforeinstallprompt', (e) => e.stopImmediatePropagation(), true);` });
    await s.eval(`location.reload()`);
    await s.until(`!!document.querySelector('.name-btn') && 'standalone' in navigator`, 'Safari: signed in');
    await s.eval(`document.querySelector('.name-btn').click()`);
    const safariItems = await s.eval(`[...document.querySelectorAll('.menu-item')].map((x) => x.textContent)`);
    h.check('in Safari the account menu says Add to Home Screen', safariItems.includes(t('menu.addToHome')), JSON.stringify({ items: safariItems, standalone: await s.eval(`'standalone' in navigator`) }));
    await s.eval(`[...document.querySelectorAll('.menu-item')].find((x) => x.textContent === ${q('menu.addToHome')})?.click()`);
    h.check('in Safari the menu explains Add to Home Screen in a dialog, with its steps and that it is a new device', await s.eval(`(() => { const d = document.querySelector('.dialog'); return !!d && d.querySelectorAll('.steps li').length === 3 && d.textContent.includes(${q('home.newDeviceTitle')}); })()`));
    await s.shot('pwa-home-steps-phone', 390, 844);
    await h.closePage(s);

    // Opened from the Home Screen: signed in apart from Safari, said on the sign-in page; the
    // device is named as the app.
    const a = await h.contextPage();
    await a.send('Page.addScriptToEvaluateOnNewDocument', { source: `Object.defineProperty(Navigator.prototype, 'standalone', { get: () => true, configurable: true });` });
    await a.send('Page.navigate', { url: h.opts.url });
    await a.until(`!!document.querySelector('form.login')`, 'Home Screen app: sign in');
    h.check('the Home Screen app says on its sign-in page that it signs in apart from Safari', await a.eval(`(() => { const n = document.querySelector('form.login .note-box'); return !!n && n.textContent.includes('Safari'); })()`));
    h.check('…its menu has no way to install it again, and its device is named as the app', await a.eval(`import('./js/auth.js').then((m) => m.deviceLabel().endsWith(${JSON.stringify(`· ${t('device.app')}`)}))`));
    await a.shot('pwa-home-app-login-phone', 390, 844);
    await h.dropContext(a);
  },
};
