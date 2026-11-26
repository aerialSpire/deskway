// The host's panel (host/src/ui/panel.html, with design.css, lang.js and page.js built in as the host does), in the
// test browser at its window size (380 × 700) and in a short window: every state in each
// language, its layout, and what it sends to the host. Only the panel page is captured.
import { readFileSync, readdirSync } from 'node:fs';
import { sleep, lit, LANGS, HOST_TEXTS } from './harness.mjs';

const root = new URL('../../', import.meta.url);
const read = (p) => readFileSync(new URL(p, root), 'utf8');

// The page as the host builds it (its languages are the catalogs'), and its texts as the host's page script gives them.
const page = () => read('host/src/ui/panel.html').replace('<!--design-->', `<style>${read('web/static/design.css')}</style>`).replace('<!--lang-menu-->', `<script>${read('host/src/ui/lang.js')}</script>`)
  .replace('<!--page-->', `<script>const LANGUAGES = ${JSON.stringify(readdirSync(new URL('host/i18n/', root)).filter((f) => f.endsWith('.json')).map((f) => f.slice(0, -5)).sort())};\n${read('host/src/ui/page.js')}</script>`);
// The on-screen strip, built the same way.
const overlay = () => read('host/src/ui/overlay.html').replace('<!--design-->', `<style>${read('web/static/design.css')}</style>`)
  .replace('<!--page-->', `<script>const LANGUAGES = [];\n${read('host/src/ui/page.js')}</script>`);

const NOW = Math.floor(Date.now() / 1000);
const device = (i, extra = {}) => ({ id: `d${i}`, name: `Device ${i} · Chrome`, used: i % 4 ? NOW - i * 7200 : 0, controlling: false, ...extra });
const devices = (n) => Array.from({ length: n }, (_, i) => device(i + 1));
const base = (extra = {}) => ({
  enrolled: true, service: true, service_running: true, service_checked: true, name: 'Office PC', auto_lock: false,
  online: true, controller: null, autostart: true, prevent_sleep: true, asks: [], pair_qr: null, max_devices: 50,
  devices: [device(1), device(2, { controlling: true, name: 'Windows PC · Chrome' }), device(3), device(4)], ...extra,
});
const QR = '<svg viewBox="0 0 10 10"><rect width="10" height="10" fill="#fff"/><rect x="1" y="1" width="3" height="3"/></svg>';

// The layout of what is showing: inside the window, the bars in place, nothing cut off.
const LAYOUT = `(() => {
  const shown = (e) => e.offsetParent !== null || getComputedStyle(e).position === 'fixed';
  const all = [...document.querySelectorAll('body *')].filter((e) => shown(e) && e.getBoundingClientRect().width > 0);
  const outside = all.filter((e) => { const r = e.getBoundingClientRect(); return r.left < -0.5 || r.right > innerWidth + 0.5; }).map((e) => e.id || e.className || e.tagName);
  const clipped = all.filter((e) => e.children.length === 0 && e.textContent.trim() && getComputedStyle(e).textOverflow !== 'ellipsis' && e.scrollWidth > e.clientWidth + 1).map((e) => e.textContent.trim().slice(0, 30));
  const view = [...document.querySelectorAll('.view')].find((v) => !v.hidden);
  const bars = view ? [...view.querySelectorAll(':scope > .head, :scope > .foot')].filter((b) => !b.hidden).every((b) => { const r = b.getBoundingClientRect(); return r.top >= 0 && r.bottom <= innerHeight + 0.5; }) : true;
  const pageScrolls = document.scrollingElement.scrollHeight > innerHeight + 1;
  return { ok: !outside.length && !clipped.length && bars && !pageScrolls, outside, clipped, bars, pageScrolls };
})()`;

export const area = {
  name: 'panel',
  needs: [],
  async run(h) {
    for (const lang of LANGS) {
      const p = await h.contextPage({ lang: null });
      await p.send('Page.addScriptToEvaluateOnNewDocument', { source: `window.ipc = { postMessage: (m) => (window.__sent = window.__sent || []).push(JSON.parse(m)) }; window.I18N = ${JSON.stringify({ lang, dict: HOST_TEXTS[lang] })};` });
      await p.metrics(380, 700, false, 1);
      await p.send('Page.navigate', { url: `data:text/html;charset=utf-8;base64,${Buffer.from(page()).toString('base64')}` });
      await p.until(`typeof window.render === 'function'`, `${lang}: panel page`);
      const render = (s) => p.eval(`(() => { window.render(${lit({ keep_awake_min_battery: 15, ...s })}); return true; })()`);
      const sent = () => p.eval(`JSON.stringify(window.__sent || [])`).then(JSON.parse);
      const clearSent = () => p.eval(`window.__sent = []`);
      const layout = async (where) => {
        const l = await p.eval(LAYOUT);
        h.check(`panel ${lang}: ${where}: inside the window, bars in place, nothing cut off, only the content scrolls`, l.ok, JSON.stringify(l));
      };
      const t = HOST_TEXTS[lang];

      // Signed out: the title, the language at the top right, no bottom bar.
      await render({ enrolled: false });
      h.check(`panel ${lang}: signed out: the title is the sign-in's, the language sits at the top right`, await p.eval(`document.querySelector('#title').textContent === ${JSON.stringify(t['login.title'])} && document.querySelector('#foot').hidden && (() => { const r = document.querySelector('#lang').getBoundingClientRect(); return r.left > innerWidth / 2 && r.top < 60; })()`));
      await layout('signed out');
      await p.shot(`panel-${lang}-login`, 380, 700);
      // The language is the client's menu, not a native list: each in its own name, the current checked.
      const other = LANGS.find((l) => l !== lang);
      await clearSent();
      await p.eval(`document.querySelector('#lang .lang-btn').click()`);
      h.check(`panel ${lang}: the language opens the shared menu, every language in its own name, the current one checked`, await p.eval(`(() => {
        const items = [...document.querySelectorAll('#lang .menu-item')];
        return !document.querySelector('select') && !document.querySelector('#lang .menu').hidden && items.length === 3
          && items.map((i) => i.lang).join() === 'en,zh-Hans,zh-Hant' && items.filter((i) => i.getAttribute('aria-checked') === 'true').map((i) => i.lang).join() === ${JSON.stringify(lang)};
      })()`));
      await p.shot(`panel-${lang}-language`, 380, 700);
      await p.eval(`document.querySelector('#lang .menu-item[lang=${JSON.stringify(other)}]').click()`);
      h.check(`panel ${lang}: choosing another language tells the host and closes the menu`, JSON.stringify(await sent()) === JSON.stringify([{ cmd: 'lang', lang: other }]) && await p.eval(`document.querySelector('#lang .menu').hidden`), JSON.stringify(await sent()));

      // Signed in: settings, then at most three devices, the one in control first.
      await render(base({ devices: [...devices(11), device(12, { controlling: true, name: 'Windows PC · Chrome' })] }));
      h.check(`panel ${lang}: keep-awake shows the 15% battery threshold`, await p.eval(`document.querySelector('#prevent-sleep-note').textContent === ${lit(t['panel.prevent_sleep_note'].replace('{percent}', '15'))}`));
      const overview = await p.eval(`[...document.querySelectorAll('#overview .dev b')].map((b) => b.textContent)`);
      h.check(`panel ${lang}: the overview lists three devices, the one in control first, then the most recently used`, overview.length === 3 && overview[0] === 'Windows PC · Chrome' && overview[1] === 'Device 1 · Chrome', JSON.stringify(overview));
      h.check(`panel ${lang}: twelve devices: "see all" with the count`, await p.eval(`!document.querySelector('#see-all').hidden && document.querySelector('#see-all').textContent.includes('12')`));
      await layout('signed in');
      await p.shot(`panel-${lang}-home`, 380, 700);

      // A short window: the bars stay, the content scrolls between them.
      await p.metrics(380, 460, false, 1);
      await layout('a short window');
      h.check(`panel ${lang}: a short window: the settings and devices scroll between the bars`, await p.eval(`(() => { const m = document.querySelector('#home .main'); return m.scrollHeight > m.clientHeight; })()`));
      await p.metrics(380, 700, false, 1);

      // Removing one, in place.
      await clearSent();
      await p.eval(`document.querySelector('#overview .dev:nth-child(2) .btn').click()`);
      h.check(`panel ${lang}: removing a device asks in place, naming it`, await p.eval(`!!document.querySelector('#overview .confirm') && document.querySelector('#overview .confirm .title').textContent.includes('Device 1')`));
      await p.eval(`document.querySelector('#overview .confirm .btn.danger').click()`);
      h.check(`panel ${lang}: confirmed, the host is told which device`, JSON.stringify(await sent()) === JSON.stringify([{ cmd: 'remove-device', id: 'd1' }]), JSON.stringify(await sent()));

      // All devices: a page of their own, searchable from ten on; Esc goes back.
      await p.eval(`document.querySelector('#see-all').click()`);
      h.check(`panel ${lang}: "see all" opens the device page with every device and a search box`, await p.eval(`!document.querySelector('#devices-view').hidden && document.querySelector('#home').hidden && document.querySelectorAll('#all-devices .dev').length === 12 && !document.querySelector('#search-wrap').hidden`));
      await layout('the device page');
      await p.shot(`panel-${lang}-devices`, 380, 700);
      // Titles are one line, the count after them whole, in every language.
      const oneLine = (sel) => `(() => { const e = document.querySelector(${JSON.stringify(sel)}), n = e.querySelector('.n'); const line = parseFloat(getComputedStyle(e).lineHeight) || e.getBoundingClientRect().height; return e.getBoundingClientRect().height <= line + 1 && (!n || (n.scrollWidth <= n.clientWidth + 1 && n.getBoundingClientRect().right <= e.getBoundingClientRect().right + 0.5)); })()`;
      h.check(`panel ${lang}: the device page's title is one line, its count whole`, await p.eval(oneLine('#devices-view h1')));
      await p.eval(`(() => { const s = document.querySelector('#search'); s.value = 'windows'; s.dispatchEvent(new Event('input')); })()`);
      h.check(`panel ${lang}: the search finds by name`, await p.eval(`document.querySelectorAll('#all-devices .dev').length === 1`));
      await p.eval(`document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))`);
      h.check(`panel ${lang}: Esc goes back from the device page`, await p.eval(`document.querySelector('#devices-view').hidden && !document.querySelector('#home').hidden`));

      const longName = 'Controller'.repeat(6);
      await render(base({ devices: [device(1, { name: longName }), device(2)] }));
      await clearSent();
      h.check(`panel ${lang}: two devices can be managed together`, await p.eval(`!document.querySelector('#see-all').hidden`));
      await layout('a long device name');
      await p.eval(`document.querySelector('#see-all').click(); document.querySelector('#unpair').click()`);
      h.check(`panel ${lang}: removing all opens a confirmation dialog`, await p.eval(`!document.querySelector('#unpair-overlay').hidden`));
      await layout('removing all');
      await p.eval(`document.querySelector('#unpair-confirm').click()`);
      h.check(`panel ${lang}: confirmed, the host is told to remove all devices`, JSON.stringify(await sent()) === JSON.stringify([{ cmd: 'unpair' }]));
      await render(base({ devices: [] }));
      h.check(`panel ${lang}: removing the last devices returns to the overview immediately`, await p.eval(`document.querySelector('#devices-view').hidden && !document.querySelector('#home').hidden`));

      // A computer's name too long for the title ends in …; the title stays one line.
      await render(base({ name: 'The very long name of the computer in the upstairs study room' }));
      h.check(`panel ${lang}: a long computer name keeps the title to one line, ending in …`, await p.eval(`(() => { const e = document.querySelector('#title'); return getComputedStyle(e).textOverflow === 'ellipsis' && e.scrollWidth > e.clientWidth && e.getBoundingClientRect().height <= parseFloat(getComputedStyle(e).lineHeight) + 1; })()`));
      await layout('a long computer name');
      h.check(`panel ${lang}: the bar's language and close are the same size, both centred on the title's line`, await p.eval(`(() => {
        const r = (s) => document.querySelector(s).getBoundingClientRect();
        const lang = r('#lang .lang-btn'), close = r('#close'), title = r('#title');
        const mid = title.top + parseFloat(getComputedStyle(document.querySelector('#title')).lineHeight) / 2;
        const centre = (b) => b.top + b.height / 2;
        const globe = r('#lang .lang-btn svg');
        return lang.width === close.width && lang.height === close.height && Math.abs(centre(lang) - mid) <= 1 && Math.abs(centre(close) - mid) <= 1 && globe.width >= 15 && globe.height >= 15;
      })()`), await p.eval(`JSON.stringify(['#lang .lang-btn', '#close', '#title'].map((s) => { const b = document.querySelector(s).getBoundingClientRect(); return [s, b.top, b.height, b.width]; }))`));

      // The account at the foot, the server's address under the pointer; one name when both are the same.
      await render(base({ account: { user: 'me', display: 'Alex' }, server: 'https://deskway.example' }));
      h.check(`panel ${lang}: the foot shows the account and, under the pointer, its server`, await p.eval(`(() => { const a = document.querySelector('#account'); return !a.hidden && a.textContent === ${JSON.stringify(t['panel.account'].replace('{display}', 'Alex').replace('{user}', 'me'))} && a.title === 'https://deskway.example'; })()`));
      await render(base({ account: { user: 'me', display: 'me' }, server: 'https://deskway.example' }));
      h.check(`panel ${lang}: the same name twice shows once`, await p.eval(`document.querySelector('#account').textContent === 'me'`));

      // No device yet.
      await render(base({ devices: [] }));
      h.check(`panel ${lang}: no device yet: the empty state with its own button, no list`, await p.eval(`!document.querySelector('#no-devices').hidden && document.querySelector('#add').hidden && !document.querySelector('#overview').children.length`));
      await layout('no device yet');
      await p.shot(`panel-${lang}-empty`, 380, 700);

      // Adding: the QR code in a dialog, closed by the device appearing, which is then named.
      await clearSent();
      await render(base());
      await p.eval(`document.querySelector('#add').click()`);
      await render(base({ pair_qr: QR }));
      h.check(`panel ${lang}: adding asks the host for a pairing window and shows its QR code in a dialog`, await p.eval(`!document.querySelector('#add-overlay').hidden && !!document.querySelector('#qr-img svg')`) && (await sent()).some((m) => m.cmd === 'pair'));
      await layout('adding');
      await p.shot(`panel-${lang}-add`, 380, 700);
      await render(base({ pair_qr: QR, devices: [...base().devices, device(9, { name: 'iPhone · Safari' })] }));
      h.check(`panel ${lang}: the new device closes the dialog, is named, and the pairing window is closed`, await p.eval(`document.querySelector('#add-overlay').hidden && !document.querySelector('#toast').hidden && document.querySelector('#toast').textContent.includes('iPhone')`) && (await sent()).some((m) => m.cmd === 'pair-stop'));

      // Full: no QR code, the reason and the way to the devices.
      await clearSent();
      await render(base({ devices: devices(50) }));
      await p.eval(`document.querySelector('#add').click()`);
      h.check(`panel ${lang}: fifty devices: adding explains the limit instead of pairing`, await p.eval(`!document.querySelector('#full-overlay').hidden && document.querySelector('#full-n').textContent === '50'`) && !(await sent()).some((m) => m.cmd === 'pair'));
      await layout('full');
      await p.shot(`panel-${lang}-full`, 380, 700);
      await p.eval(`document.querySelector('#full-manage').click()`);
      h.check(`panel ${lang}: "manage devices" opens the device page`, await p.eval(`document.querySelector('#full-overlay').hidden && !document.querySelector('#devices-view').hidden`));
      await p.eval(`document.querySelector('#back').click()`);

      // Asked to let a device in: over everything, one at a time; Esc denies.
      await render(base());
      await p.eval(`document.querySelector('#add').click()`);
      await clearSent();
      const ask = (i) => ({ session: `s${i}`, name: 'iPhone · Safari', from: '', code: '482913' });
      await render(base({ pair_qr: QR, asks: [ask(1), ask(2)] }));
      h.check(`panel ${lang}: a request to be let in shows over the panel with its code, and says another waits`, await p.eval(`!document.querySelector('#ask-overlay').hidden && document.querySelector('#ask-code').textContent === '482 913' && !document.querySelector('#ask-more').hidden`));
      await layout('a request to be let in');
      await p.shot(`panel-${lang}-ask`, 380, 700);
      h.check(`panel ${lang}: a request stays above an open QR dialog`, await p.eval(`document.elementFromPoint(innerWidth / 2, innerHeight / 2).closest('.overlay')?.id === 'ask-overlay'`));
      await p.eval(`document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))`);
      h.check(`panel ${lang}: Esc denies the request`, JSON.stringify(await sent()) === JSON.stringify([{ cmd: 'ask', session: 's1', on: false }]), JSON.stringify(await sent()));

      // Signing in and out is the panel's, on Windows too (the service installed): the sign-in
      // names each field above it, as the web's does; "Sign out" asks first, in place. The same in
      // every language: checked in the first.
      if (lang === LANGS[0]) {
        await render({ enrolled: false, service: true });
        h.check('panel: the sign-in names each field above it, with no stand-in text inside', await p.eval(`['server', 'user', 'password'].every((id) => { const i = document.getElementById(id), l = i.closest('label'); return !!l && l.textContent.trim().length > 0 && !i.placeholder && l.firstElementChild.getBoundingClientRect().bottom <= i.getBoundingClientRect().top + 1; })`));
        await render(base());
        await clearSent();
        h.check('panel: with the service installed, Sign out is offered', await p.eval(`!document.querySelector('#foot').hidden && !document.querySelector('#leave').hidden`));
        await p.eval(`document.querySelector('#leave').click()`);
        h.check('panel: Sign out asks first, in place, and sends nothing yet', await p.eval(`!document.querySelector('#leave-confirm').hidden && document.querySelector('#leave').hidden`) && !(await sent()).length);
        await layout('signing out, asked first');
        await p.shot(`panel-${lang}-sign-out`, 380, 700);
        await p.eval(`document.querySelector('#leave-confirm .btn:not(.danger)').click()`);
        h.check('panel: Cancel keeps it signed in', await p.eval(`document.querySelector('#leave-confirm').hidden && !document.querySelector('#leave').hidden`) && !(await sent()).length);
        await p.eval(`document.querySelector('#leave').click(); document.querySelector('#leave-confirm .btn.danger').click()`);
        h.check('panel: confirmed, the host is told to sign out', JSON.stringify(await sent()) === JSON.stringify([{ cmd: 'leave' }]), JSON.stringify(await sent()));
      }

      // AI clients here: the line says who controls this computer now (with "Disconnect"), that they
      // may (with "Stop AI control"), or that they were stopped here; the button asks the host to stop them.
      for (const [name, s, line, stop] of [
        ['ai-working', { ai_client: 'Claude Code', ai_state: 'on' }, t['panel.ai_using'].replace('{name}', 'Claude Code'), t['overlay.disconnect']],
        ['ai-allowed', { ai_state: 'on' }, t['panel.ai_on'], t['panel.stop_ai']],
        ['ai-stopped', { ai_state: 'stopped' }, t['panel.ai_stopped'], ''],
        ['ai-off', { ai_state: 'off' }, '', ''],
      ]) {
        await render(base(s));
        const got = await p.eval(`JSON.stringify({ line: document.querySelector('#ai-row').hidden ? '' : document.querySelector('#ai-line').textContent, stop: document.querySelector('#stop-ai').hidden ? '' : document.querySelector('#stop-ai').textContent })`).then(JSON.parse);
        h.check(`panel ${lang}: ${name}: the AI line says so, and its button shows only while AI may control it`, got.line === line && got.stop === stop, JSON.stringify(got));
        if (name !== 'ai-off') {
          await layout(name);
          await p.shot(`panel-${lang}-${name}`, 380, 700);
        }
      }
      await render(base({ ai_client: 'Claude Code', ai_state: 'on' }));
      await clearSent();
      await p.eval(`document.querySelector('#stop-ai').click()`);
      h.check(`panel ${lang}: Stop AI control asks the host to stop AI clients`, (await sent()).some((m) => m.cmd === 'stop-ai'), JSON.stringify(await sent()));

      // AppKit uses the visible title's rectangle, so controls stay clickable as the labels reflow.
      const mac = { screen: false, accessibility: false, full_disk_access: false, login: 'approval' };
      for (const [state, granted] of [['off', false], ['on', true], ['off again', false]]) {
        await render(base({ service: false, mac: { ...mac, screen: true, accessibility: true, full_disk_access: granted } }));
        const access = t[granted ? 'mac.full_disk_access_on' : 'mac.full_disk_access_off'];
        h.check(`panel ${lang}: Full Disk Access is ${state} and the host is online`, await p.eval(`
          !document.querySelector('#mac-permissions').hidden && document.querySelector('#permissions').hidden
          && document.querySelector('#status').textContent === ${lit(t['status.online'])}
          && document.querySelector('#full-disk-access-label').textContent === ${lit(t['mac.full_disk_access'])}
          && document.querySelector('#full-disk-access-status').textContent === ${lit(access)}`));
      }
      await clearSent();
      await render(base({ service: false, mac: { ...mac, screen: true, accessibility: true } }));
      await p.eval(`document.querySelector('#full-disk-access-settings').click()`);
      h.check(`panel ${lang}: Full Disk Access opens its settings`, (await sent()).some((m) => m.cmd === 'full-disk-access-settings'));
      await layout('optional Full Disk Access');

      await render(base({ mac }));
      h.check(`panel ${lang}: restart appears after requesting Screen Recording`,
        await p.eval(`document.querySelector('#reopen-for-permissions').hidden`));
      await render(base({ mac: { ...mac, permission_request: 'screen' } }));
      await clearSent();
      h.check(`panel ${lang}: the Mac can quit and reopen after a Screen Recording request`,
        await p.eval(`(() => { const b = document.querySelector('#reopen-for-permissions'); if (b.hidden) return false; b.click(); return true; })()`) &&
        (await sent()).some(m => m.cmd === 'reopen-for-permissions'));
      await layout('Mac screen permission request');
      await render({ enrolled: false, mac });
      h.check(`panel ${lang}: signing out dismisses pending device dialogs and shows Mac login-item cleanup`, await p.eval(`!document.querySelector('#login-cleanup').hidden && !document.querySelector('.overlay:not([hidden])')`));
      await clearSent();
      await render(base({ name: 'Deskway Mac for native panel testing', service: false, mac, devices: devices(10) }));
      const region = async () => {
        await p.until(`(window.__sent || []).some(m => m.cmd === 'drag-region')`, 'Mac drag region');
        return (await sent()).filter((m) => m.cmd === 'drag-region').at(-1).rect;
      };
      const homeRect = await region();
      const langLeft = await p.eval(`document.querySelector('#lang').getBoundingClientRect().left`);
      h.check(`panel ${lang}: Mac drag region covers the title and leaves language and window controls clickable`, homeRect?.[0] < homeRect?.[2] && homeRect?.[2] <= langLeft);
      await layout('Mac permissions and login approval');
      await clearSent();
      await p.eval(`document.querySelector('#lang .lang-btn').click()`);
      h.check(`panel ${lang}: the language menu suspends Mac title dragging`, await region() === null);
      await clearSent();
      await p.eval(`document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))`);
      h.check(`panel ${lang}: Esc closes the language menu and restores Mac title dragging`,
        await p.eval(`document.querySelector('#lang .menu').hidden`) && JSON.stringify(await region()) === JSON.stringify(homeRect));

      await p.eval(`document.querySelector('#see-all').click()`);
      await p.until(`window.__sent.filter(m => m.cmd === 'drag-region').length >= 2`, 'Mac devices drag region');
      const devicesRect = await region();
      const [backRight, closeLeft] = await p.eval(`[document.querySelector('#back').getBoundingClientRect().right, document.querySelector('#close2').getBoundingClientRect().left]`);
      h.check(`panel ${lang}: Mac device page drag region leaves back and close clickable`, backRight <= devicesRect?.[0] && devicesRect?.[2] <= closeLeft);
      h.check(`panel ${lang}: exactly ten devices can be searched`, await p.eval(`!document.querySelector('#search-wrap').hidden`));
      await clearSent();
      await p.eval(`document.querySelector('#add2').click()`);
      h.check(`panel ${lang}: a dialog suspends Mac title dragging`, await region() === null);

      h.check(`panel ${lang}: no script errors`, !p.errors.length, p.errors.join(' | '));
      await sleep(100);
      await h.dropContext(p);

      // The on-screen strip (overlay.html): who controls this computer, a person and an AI
      // client each with their own way to end it.
      const o = await h.contextPage({ lang: null });
      await o.send('Page.addScriptToEvaluateOnNewDocument', { source: `window.ipc = { postMessage: (m) => (window.__sent = window.__sent || []).push(JSON.parse(m)) }; window.I18N = ${JSON.stringify({ lang, dict: HOST_TEXTS[lang] })};` });
      await o.metrics(900, 120, false, 1);
      await o.send('Page.navigate', { url: `data:text/html;charset=utf-8;base64,${Buffer.from(overlay()).toString('base64')}` });
      await o.until(`typeof window.setState === 'function'`, `${lang}: the strip`);
      const long = 'Upstairs study room laptop with a very long device name';
      for (const [name, s] of [['person', { person: 'Windows PC · Chrome', ai: null }], ['ai', { person: null, ai: 'Claude Code' }], ['both', { person: 'Windows PC · Chrome', ai: 'Claude Code' }], ['long', { person: long, ai: long }]]) {
        await o.eval(`(() => { window.setState(${JSON.stringify(s)}); return true; })()`);
        const shown = await o.eval(`JSON.stringify({ text: document.querySelector('#person').hidden ? '' : document.querySelector('#text').textContent, ai: document.querySelector('#ai-row').hidden ? '' : document.querySelector('#ai').textContent, end: !document.querySelector('#person').hidden, stop: !document.querySelector('#ai-row').hidden })`).then(JSON.parse);
        const want = { text: s.person ? t['overlay.controlling'].replace('{name}', s.person) : '', ai: s.ai ? t['overlay.ai'].replace('{name}', s.ai) : '', end: !!s.person, stop: !!s.ai };
        h.check(`strip ${lang}: ${name}: who is named, each in a row of their own with their way to end it`, JSON.stringify(shown) === JSON.stringify(want), JSON.stringify(shown));
        h.check(`strip ${lang}: ${name}: at most 560 wide, names give way to the buttons, the buttons whole and in one column`, await o.eval(`(() => {
          const bar = document.querySelector('#bar').getBoundingClientRect();
          const rows = [...document.querySelectorAll('#bar .who')].filter((r) => !r.hidden);
          const buttons = rows.map((r) => r.querySelector('button:last-of-type'));
          return bar.width <= 560.5 && buttons.every((b) => b.scrollWidth <= b.clientWidth + 1 && b.getBoundingClientRect().right <= bar.right + 0.5)
            && buttons.every((b) => Math.abs(b.getBoundingClientRect().right - buttons[0].getBoundingClientRect().right) < 1)
            && new Set(rows.map((r) => Math.round(r.getBoundingClientRect().top))).size === rows.length;
        })()`));
        await o.shot(`strip-${lang}-${name}`, 900, 120);
      }
      h.check(`strip ${lang}: no script errors`, !o.errors.length, o.errors.join(' | '));
      await h.dropContext(o);
    }
  },
};
