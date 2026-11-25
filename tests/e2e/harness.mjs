// The shared harness of the end-to-end tests: a headless browser driven over the Chrome
// DevTools Protocol, pages of the client, the test host's tools, and the checks' results.
// Tests read and drive the client through `window.__deskway` (web/static/js/test-api.js) and
// its elements. Reports video metrics only; it never saves screen frames.
import { spawn, spawnSync } from 'node:child_process';
import { readFileSync, mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { createHash } from 'node:crypto';
import { connect as pipeConnect } from 'node:net';
import { dirname, join, resolve } from 'node:path';

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---- words ----

// The client's languages.
export const LANGS = ['en', 'zh-Hans', 'zh-Hant'];

// The catalogs of the web client and of the host, each language as the programs load it: over
// English, which fills what it lacks (web/static/js/i18n.js, host/src/i18n.rs).
const readJson = (path) => JSON.parse(readFileSync(new URL(`../../${path}`, import.meta.url), 'utf8'));
const catalogs = (dir) => Object.fromEntries(LANGS.map((l) => [l, { ...readJson(`${dir}/en.json`), ...readJson(`${dir}/${l}.json`) }]));
export const WEB_TEXTS = catalogs('web/static/i18n');
export const HOST_TEXTS = catalogs('host/i18n');

// The words for `key` in `lang` of `texts`, made the way web/static/js/i18n.js makes them: a
// numeric `params.n` picks the plural form, and `params` fill the {name} placeholders. A key no
// catalog has is the test's mistake, so it throws.
export function words(key, params, { lang = 'en', texts = WEB_TEXTS } = {}) {
  const cat = texts[lang];
  let s;
  if (params && typeof params.n === 'number') {
    const form = new Intl.PluralRules(lang).select(params.n);
    s = cat[`${key}.${form}`] ?? cat[`${key}.other`];
  }
  s ??= cat[key];
  if (s === undefined) throw new Error(`no text "${key}" in ${lang}`);
  return params ? s.replace(/\{(\w+)\}/g, (m, k) => (k in params ? String(params[k]) : m)) : s;
}

// Pages run in English unless a check asks for another language: `t` is the web client's words
// in English, and `q` the same quoted for an expression evaluated on the page.
export const t = (key, params) => words(key, params);
export const q = (key, params) => JSON.stringify(t(key, params));
// A value from outside the test (a computer's id or name) as a literal in an expression evaluated on the page:
// JSON, with every character that could end a script or a line written as its escape.
export const lit = (value) => JSON.stringify(value).replace(/[<>\/\u2028\u2029]/g, (c) => `\\u${c.charCodeAt(0).toString(16).padStart(4, '0')}`);

// The longest fixed part of a text with placeholders: what the page shows whatever fills them.
export const fixed = (key, opts) => words(key, undefined, opts).split(/\{\w+\}/).map((x) => x.trim()).reduce((a, b) => (b.length > a.length ? b : a));

const processIdentity = (pid) => spawnSync('/bin/ps', ['-p', String(pid), '-o', 'lstart=,comm='], { encoding: 'utf8' }).stdout?.trim();

// A computer's page: pressing a screen takes over (the first display, or the only picture).
// SCREENS is true once it shows; TAKE_OVER presses it.
const SCREEN = `document.querySelector('.display-tile, .preview.clickable')`;
export const SCREENS = `!!${SCREEN}`;
export const TAKE_OVER = `${SCREEN}.click()`;

// Options from the command line (see run.mjs). Ports are parameters, so several runs can go
// side by side: --devtools-port for the browser, --url for the server.
export function options(argv = process.argv) {
  const arg = (name) => {
    const i = argv.indexOf(`--${name}`);
    return i < 0 ? undefined : argv[i + 1];
  };
  const need = (name) => {
    const v = arg(name);
    if (!v) throw new Error(`--${name} is required`);
    return v;
  };
  return {
    browser: need('browser'),
    url: need('url').replace(/\/$/, ''),
    user: arg('user'),
    password: arg('password-file') ? readFileSync(arg('password-file'), 'utf8').trim() : undefined,
    superPassword: arg('super-file') ? readFileSync(arg('super-file'), 'utf8').trim() : undefined,
    hostExe: arg('host-exe'),
    hostHome: arg('home'),
    outDir: arg('out'),
    // Optional: the server's executable and data file, for restarting it during a session.
    serverExe: arg('server-exe'),
    serverState: arg('server-state'),
    browserArgs: argv.filter((v) => v.startsWith('--browser-arg=')).map((v) => v.slice('--browser-arg='.length)),
    port: Number(arg('devtools-port') || 9333),
    areas: arg('areas') ? arg('areas').split(',').map((x) => x.trim()).filter(Boolean) : null,
  };
}

// One page (or the browser itself) over the DevTools protocol.
export class Page {
  constructor(ws, h) { this.ws = ws; this.h = h; this.id = 0; this.waiting = new Map(); this.errors = []; }

  static async open(target, h) {
    const ws = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
    const p = new Page(ws, h);
    ws.onmessage = (e) => {
      const m = JSON.parse(e.data);
      if (m.id && p.waiting.has(m.id)) { p.waiting.get(m.id)(m); p.waiting.delete(m.id); }
      if (m.method === 'Runtime.exceptionThrown') p.errors.push(m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text);
    };
    await p.send('Page.enable');
    await p.send('Runtime.enable');
    return p;
  }

  // A command the browser never answers fails the run instead of hanging it.
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((res, rej) => {
      const timer = setTimeout(() => { this.waiting.delete(id); rej(new Error(`no answer to ${method} within 60 s`)); }, 60000);
      this.waiting.set(id, (m) => { clearTimeout(timer); res(m); });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  // Runs `code` in the page as it is.
  async eval(code) {
    const r = await this.send('Runtime.evaluate', { expression: code, awaitPromise: true, returnByValue: true });
    if (r.result?.exceptionDetails) throw new Error(JSON.stringify(r.result.exceptionDetails));
    return r.result?.result?.value;
  }

  // The value of an expression with `d` (the test API) and `s` (the open session, or null).
  v(expr) {
    return this.eval(`(async () => { const d = window.__deskway; const s = d && d.session(); return (${expr}); })()`);
  }

  async until(expr, label, ms = 15000) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      if (await this.v(expr).catch(() => false)) return;
      await sleep(150);
    }
    const body = await this.eval('document.body.innerText.slice(0, 300)');
    throw new Error(`timed out waiting for: ${label}\npage: ${body}\nerrors: ${this.errors.join(' | ')}`);
  }

  // Clicks the first `sel` whose text contains `text` (`exact`: is `text`). False when none.
  clickText(sel, text, { exact = false } = {}) {
    const match = exact ? `e.textContent === ${JSON.stringify(text)}` : `e.textContent.includes(${JSON.stringify(text)})`;
    return this.eval(`(() => { const b = [...document.querySelectorAll(${JSON.stringify(sel)})].find((e) => ${match}); if (!b) return false; b.click(); return true; })()`);
  }

  metrics(width, height, mobile = false, scale = mobile ? 3 : 1) {
    return this.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: scale, mobile });
  }

  // Layout screenshots hide all remote pixels, so no real screen content reaches the file:
  // the video, the list thumbnails and every per-display preview. Any new picture of the remote
  // screen must be added here before it can appear in a screenshot.
  async shot(name, width, height) {
    // A tab in the background never finishes a screenshot.
    if (await this.eval('document.hidden')) await this.send('Page.bringToFront');
    await this.metrics(width, height, false, 1);
    // Set directly on the elements: the page's CSP blocks injected style sheets.
    const hidden = await this.eval(`(() => { const all = [...document.querySelectorAll('video, img[data-thumb], img[data-screen-thumb]')]; all.forEach((e) => { e.style.visibility = 'hidden'; }); return all.every((e) => getComputedStyle(e).visibility === 'hidden'); })()`);
    if (!hidden) throw new Error('could not hide remote pixels; refusing to take a screenshot');
    await sleep(200);
    const r = await this.send('Page.captureScreenshot', { format: 'png' });
    await this.eval(`document.querySelectorAll('video, img[data-thumb], img[data-screen-thumb]').forEach((e) => { e.style.visibility = ''; })`);
    if (this.h.opts.outDir) writeFileSync(join(this.h.opts.outDir, `${name}.png`), Buffer.from(r.result.data, 'base64'));
  }

  // A platform authenticator that always passes user verification, like Touch ID that says yes.
  async addAuthenticator() {
    await this.send('WebAuthn.enable', { enableUI: false });
    const r = await this.send('WebAuthn.addVirtualAuthenticator', { options: {
      protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true,
      isUserVerified: true, automaticPresenceSimulation: true,
    } });
    this.authId = r.result.authenticatorId;
  }

  async passkeys() { return (await this.send('WebAuthn.getCredentials', { authenticatorId: this.authId })).result.credentials; }

  // As if the passkey had synced to another device.
  async copyPasskeysTo(to) {
    for (const credential of await this.passkeys()) await to.send('WebAuthn.addCredential', { authenticatorId: to.authId, credential });
  }
}

// Pages start in a known language (English unless asked) with the test API on, both
// set before the client loads: most checks read the page's words. Set again on every load.
// The test host runs view-only; sessions take every cap from its hello so the input and file
// interfaces are tested (the lifecycle area checks the real caps).
export const ALL_CAPS = JSON.parse(readFileSync(new URL('../../proto/messages.json', import.meta.url), 'utf8')).capabilities.filter((c) => c.who === 'host').map((c) => c.name);
const startScript = (lang) => `try { localStorage.setItem('deskway.test', '1'); localStorage.setItem('deskway.test.caps', ${JSON.stringify(JSON.stringify(ALL_CAPS))}); ${lang ? `localStorage.setItem('deskway.lang', ${JSON.stringify(lang)});` : ''} } catch {}`;

// The toolbar's layout, in the session open on the page: everything inside the window, nothing
// overlapping or cut off; how many rows; whether the buttons share their row evenly.
// The toolbar's buttons are words on one line, never wrapped; a narrow window's row of them
// slides sideways when they do not fit, the cut side fading (style.css, Toolbar).
export const toolbarLayout = `(() => {
  const bar = s.el.toolbar, main = bar.querySelector('.tool-main'), group = bar.querySelector('.tool-group');
  const items = [...main.children, ...group.children].filter((e) => e.offsetParent && e.getBoundingClientRect().width > 0 && !e.classList.contains('tool-sep') && !e.classList.contains('tool-group'));
  // Buttons slid out of their row's sight are not laid over anything.
  const rowOf = getComputedStyle(group).display === 'contents' ? null : group.getBoundingClientRect();
  const seen = items.filter((e) => { const x = e.getBoundingClientRect(); return !rowOf || !group.contains(e) || (x.right > rowOf.left + 0.5 && x.left < rowOf.right - 0.5); });
  const r = seen.map((e) => e.getBoundingClientRect());
  const t = bar.getBoundingClientRect();
  const inside = t.left >= 0 && t.right <= innerWidth && t.top >= 0;
  const overlap = r.some((a, i) => r.some((b, j) => j > i && a.left < b.right - 0.5 && b.left < a.right - 0.5 && a.top < b.bottom - 0.5 && b.top < a.bottom - 0.5));
  const cut = items.filter((e) => e.scrollWidth > e.clientWidth + 1 || e.scrollHeight > e.clientHeight + 1);
  const fits = !cut.length;
  const rows = new Set(r.map((x) => Math.round(x.top + x.height / 2))).size;
  // Each button's words on one line.
  const tools = [...group.children].filter((e) => e.offsetParent);
  const oneLine = tools.every((e) => { const g = document.createRange(); g.selectNodeContents(e); const m = [...g.getClientRects()].filter((x) => x.width > 0).map((x) => x.top + x.height / 2); return Math.max(...m) - Math.min(...m) < 8; });
  // More buttons than their row holds: it slides, and says so by fading where they go on.
  const slides = !rowOf || group.scrollWidth <= group.clientWidth + 1 || group.classList.contains('fade-end') || group.classList.contains('fade-start');
  // An AI client controlling too: said in a line of its own, under everything else, inside the bar.
  const a = bar.querySelector('.tool-ai'), ar = a.getBoundingClientRect();
  const aiLine = a.hidden || (ar.top >= Math.max(...r.map((x) => x.bottom)) - 0.5 && ar.left >= t.left - 0.5 && ar.right <= t.right + 0.5);
  // The computer's name and how it is connected are readable, not squeezed away or hidden.
  const shown = (e) => { const r = e.getBoundingClientRect(); return r.width >= 12 && getComputedStyle(e).clipPath === 'none' && e.scrollWidth <= e.clientWidth + 1; };
  const named = shown(bar.querySelector('.tool-name')) && shown(s.el.route);
  return { inside, overlap, fits, rows, oneLine, slides, aiLine, named, cut: cut.map((e) => e.textContent.trim() + ' ' + e.scrollWidth + 'x' + e.scrollHeight + ' in ' + e.clientWidth + 'x' + e.clientHeight) };
})()`;
export const toolbarFits = (l, narrow) => l.inside && !l.overlap && l.fits && l.named && l.oneLine && l.slides && l.aiLine && l.rows === (narrow ? 2 : 1);

// Real key events on page `p`, each with the modifiers of the keys held so far.
const MODS = { ControlLeft: 2, ControlRight: 2, MetaLeft: 4, AltLeft: 1, ShiftLeft: 8 };
const VK = { ControlLeft: 17, ControlRight: 17, MetaLeft: 91, ShiftLeft: 16, KeyA: 65, KeyC: 67, KeyV: 86, AltLeft: 18, Tab: 9 };
export function keyboardOf(p) {
  const held = new Set();
  const press = async (code, down) => {
    if (down) held.add(code); else held.delete(code);
    const modifiers = [...held].reduce((m, c) => m | (MODS[c] || 0), 0);
    const key = { ControlLeft: 'Control', ControlRight: 'Control', MetaLeft: 'Meta', AltLeft: 'Alt', ShiftLeft: 'Shift', Tab: 'Tab' }[code] || code.slice(-1).toLowerCase();
    await p.send('Input.dispatchKeyEvent', { type: down ? 'rawKeyDown' : 'keyUp', code, key, modifiers, windowsVirtualKeyCode: VK[code], location: code.endsWith('Right') ? 2 : code.endsWith('Left') ? 1 : 0 });
  };
  return { press, held };
}

// ---- real pointer input on page `p`, at points [x, y] ----

// The middle of the first element matching `selector`, as [x, y].
export const centre = (p, selector) => p.eval(`(() => { const r = document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; })()`);

// A mouse click: the button pressed and released at `at`.
export async function click(p, [x, y], button = 'left') {
  for (const type of ['mousePressed', 'mouseReleased']) await p.send('Input.dispatchMouseEvent', { type, x, y, button, clickCount: 1 });
}

// Every finger on the glass after the change, one point each (their ids are their places in the
// list). DevTools cannot lift one finger while others stay: touchEnd lifts them all.
export const touch = (p, type, points) => p.send('Input.dispatchTouchEvent', { type, touchPoints: points.map(([x, y], id) => ({ x, y, id })) });

// One finger down at `at` and up again after `hold` ms. `during`: an expression (with `d` and
// `s`, as Page.v) the page runs while the finger is down, as if the server spoke meanwhile.
export async function tap(p, at, { hold = 0, during = null } = {}) {
  await touch(p, 'touchStart', [at]);
  if (during) { await sleep(40); await p.v(during); }
  if (hold) await sleep(hold);
  await touch(p, 'touchEnd', []);
}

// One finger down at `at`, moved by `step` ([dx, dy]) `steps` times a frame apart, then lifted.
export async function swipe(p, at, step, steps) {
  await touch(p, 'touchStart', [at]);
  for (let i = 1; i <= steps; i++) { await touch(p, 'touchMove', [[at[0] + i * step[0], at[1] + i * step[1]]]); await sleep(16); }
  await touch(p, 'touchEnd', []);
}

// `deskway-host pair` prints cli.pair_code, in any language; the code follows its fixed start.
const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
const PAIR_CODE = new RegExp(`(?:${LANGS.map((l) => escapeRe(HOST_TEXTS[l]['cli.pair_code'].split('{code}')[0])).join('|')})(\\S+)`);

// ---- signing in ----

// The sign-in page of `p`: the account name and password typed, then "Sign in".
export const signIn = (p, { user, password }) => p.eval(`(() => {
  const [u, pw] = document.querySelectorAll('.login input');
  u.value = ${JSON.stringify(user)}; pw.value = ${JSON.stringify(password)};
  document.querySelector('.login button').click();
})()`);

// A new device's first step on the waiting screen of `p`: its name (`name`, or the one filled
// in), then "Continue". Returns the name it was given.
export async function nameThisDevice(p, name) {
  await p.until(`!!document.querySelector('.name-step input')`, 'the new device asks for its name');
  return p.eval(`(() => {
    const input = document.querySelector('.name-step input');
    ${name ? `input.value = ${JSON.stringify(name)};` : ''}
    const given = input.value;
    document.querySelector('.name-step button').click();
    return given;
  })()`);
}

// The waiting screen of `p`: "No other signed-in device?" opens the super password's form (once),
// then `superPassword` is typed and sent.
export async function superIn(p, superPassword) {
  if (await p.eval(`document.querySelector('.setup').hidden`)) await p.clickText('button', t('wait.noOther'));
  await p.eval(`(() => { document.querySelector('.setup input').value = ${JSON.stringify(superPassword)}; document.querySelector('.setup button').click(); })()`);
}

// ---- test doubles in the page, for the session open on `p` ----

// A computer on the file channel, instead of the real one: it answers a put with ready, and the
// bytes with done. Its state is `window.__files`: `sent` (the messages it got), `put` (the
// latest), `bin` (bytes of it so far), `delay` (ms before each answer), `silent` (no answers).
// `s.useFileChannel(null)` puts the real channel back.
export const fakeFileChannel = (p, { silent = false } = {}) => p.eval(`(() => {
  const f = window.__files = { sent: [], put: null, bin: 0, need: 0, delay: 0, silent: ${!!silent} };
  // The session it was made for: answers that arrive after it has gone go nowhere.
  const session = __deskway.session();
  const answer = (m) => setTimeout(() => session.fromFiles(JSON.stringify(m)), f.delay);
  session.useFileChannel({
    readyState: 'open', bufferedAmount: 0, addEventListener() {},
    send(d) {
      if (typeof d !== 'string') {
        f.bin += d.byteLength;
        if (f.bin === f.need && !f.silent) answer({ t: 'done', id: f.put.id, names: f.put.items.map((i) => i.n) });
        return;
      }
      const m = JSON.parse(d);
      f.sent.push(m);
      if (m.t !== 'put') return;
      f.put = m; f.bin = 0; f.need = m.items.reduce((n, i) => n + i.s, 0);
      if (!f.silent) answer({ t: 'ready', id: m.id });
    },
  });
})()`);

// This device's clipboard, never touched: `window.__clip.items` is what it holds, as [mime,
// data] pairs; `writes` what the page put on it ([kind, text or size]); `reads` how often it was
// read; reads from number `hangFrom` on never answer (a slow permission prompt); `deny` refuses
// them. `window.__blobs` lists the size of every file the page saves.
export const clipboardStub = (p, { deny = false } = {}) => p.eval(`(() => {
  const c = window.__clip = { items: [], writes: [], reads: 0, hangFrom: Infinity, deny: ${!!deny} };
  const nav = navigator.clipboard;
  nav.read = async () => {
    c.reads += 1;
    if (c.reads >= c.hangFrom) return new Promise(() => {});
    if (c.deny) throw new DOMException('denied', 'NotAllowedError');
    return c.items.map(([type, data]) => ({ types: [type], getType: async () => (data instanceof Blob ? data : new Blob([data], { type })) }));
  };
  nav.readText = async () => '';
  nav.writeText = async (t) => { c.writes.push(['text', t]); };
  nav.write = async (items) => { for (const it of items) c.writes.push(['image', (await it.getType('image/png')).size]); };
  window.__blobs = [];
  const o = URL.createObjectURL;
  URL.createObjectURL = (b) => { window.__blobs.push(b.size); return o(b); };
})()`);

// The transfer card, on a file channel that never answers: two files under way and one more
// waiting. Checks it fits in one small line; `endTransferCard` takes it away again.
export async function transferCard(h, p, lang, where) {
  await fakeFileChannel(p, { silent: true });
  await p.v(`(s.sendFiles([new File(['a'], 'Q3 Report.PDF'), new File(['b'], 'Notes.TXT')]), s.sendFiles([new File(['c'], 'Plan.TXT')]))`);
  await p.until(`!!s.el.xferCard`, `${lang}: transfer card`, 3000);
  const fits = await p.v(`(() => { const r = s.el.xferCard.getBoundingClientRect(), st = s.el.stage.getBoundingClientRect(); return r.width <= 360 && r.left >= st.left && r.right <= st.right && r.height <= 30; })()`);
  h.check(`${lang}: the transfer card fits in one small line ${where}`, fits, await p.v(`s.el.xferCard.textContent + ' ' + Math.round(s.el.xferCard.getBoundingClientRect().width) + 'px'`));
}
export const endTransferCard = (p) => p.v(`(s.dropQueue(), s.cancelTransfer(), s.dismissTransfer(), s.useFileChannel(null))`);

// Everything a run shares: options, results, the browser, the pages made so far, and what the
// areas leave for each other: `h.a`, the first device, and `h.hostId`, the test computer (both
// from the auth area); `h.b`, that device's second tab (secondPage).
export class Harness {
  constructor(opts) {
    this.opts = opts;
    this.results = [];
    this.pages = [];
    this.devtools = `http://127.0.0.1:${opts.port}`;
  }

  check(name, ok, detail = '') {
    this.results.push({ name, ok: !!ok });
    console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`);
  }

  // Every process the tests start runs without a window (windowsHide): no console pops up on
  // the desktop of whoever runs them.
  async launch() {
    this.profile = mkdtempSync(join(tmpdir(), 'deskway-e2e-'));
    this.browser = spawn(this.opts.browser, [
      '--headless=new', `--remote-debugging-port=${this.opts.port}`, `--user-data-dir=${this.profile}`,
      '--no-first-run', '--no-default-browser-check', '--disable-extensions', '--autoplay-policy=no-user-gesture-required',
      ...this.opts.browserArgs,
      // The browser is a desktop with a mouse and no touch screen, whatever machine runs the
      // tests (a touch screen there would change pointer media queries and maxTouchPoints);
      // phone pages switch to touch by emulation. Blink values: pointer fine = 4, hover = 2.
      '--blink-settings=primaryPointerType=4,availablePointerTypes=4,primaryHoverType=2,availableHoverTypes=2',
      '--touch-events=disabled',
      'about:blank',
    ], { stdio: 'ignore', windowsHide: true });
    for (let i = 0; i < 50; i++) {
      try { await fetch(`${this.devtools}/json/version`); return; } catch { await sleep(200); }
    }
    throw new Error(`no browser on DevTools port ${this.opts.port}`);
  }

  async shutdown() {
    this.stopTestHost();
    if (this.server) { try { this.server.kill(); } catch { /* gone */ } }
    if (this.browser) this.browser.kill();
    await sleep(500);
    try { rmSync(this.profile, { recursive: true, force: true }); } catch { /* the browser may still hold files */ }
  }

  // A Mac app gets its existing privacy grants when opened by Launch Services. The launcher
  // returns only the new instance's PID. The installed instance can keep running independently.
  startTestHost() {
    const { hostExe, hostHome, url } = this.opts;
    if (process.platform !== 'darwin' || !hostExe || !hostHome || new URL(url).hostname !== 'localhost') {
      throw new Error('host-restart needs a Mac test app, --host-exe, --home, and a localhost server');
    }
    const config = JSON.parse(readFileSync(join(hostHome, 'host.json'), 'utf8'));
    if (config.server !== url || config.login_item_setup_pending || config.auto_lock) {
      throw new Error('host-restart needs a localhost test profile with login setup and auto-lock off');
    }
    const exe = resolve(hostExe);
    const app = dirname(dirname(dirname(exe)));
    if (!app.endsWith('.app') || exe !== join(app, 'Contents/MacOS', 'deskway-host')) {
      throw new Error('--host-exe must name the executable within the Mac test app');
    }
    const launcher = `
      ObjC.import('AppKit');
      function run([path, home]) {
        const workspace = $.NSWorkspace.sharedWorkspace;
        const running = ObjC.deepUnwrap(workspace.runningApplications.valueForKey('processIdentifier'));
        const config = $({ NSWorkspaceLaunchConfigurationEnvironment: { DESKWAY_HOME: home } });
        const flags = $.NSWorkspaceLaunchNewInstance | $.NSWorkspaceLaunchWithoutActivation;
        const app = workspace.launchApplicationAtURLOptionsConfigurationError($.NSURL.fileURLWithPath(path), flags, config, null);
        const pid = Number(app.processIdentifier);
        if (!pid || running.includes(pid)) throw new Error('no new test host instance');
        return pid;
      }`;
    const child = spawnSync('/usr/bin/osascript', ['-l', 'JavaScript', '-e', launcher, app, hostHome], { encoding: 'utf8' });
    if (child.status !== 0) throw new Error(`launching the test host failed: ${child.stderr.trim()}`);
    const pid = Number(child.stdout.trim());
    if (!Number.isSafeInteger(pid) || pid <= 1) throw new Error('invalid test host PID');
    const identity = processIdentity(pid);
    if (!identity?.endsWith(` ${exe}`)) throw new Error('the new test host exited or ran from another path');
    this.testHost = { pid, identity };
  }

  stopTestHost() {
    const host = this.testHost;
    this.testHost = null;
    if (!host || processIdentity(host.pid) !== host.identity) return;
    try { process.kill(host.pid, 'SIGKILL'); }
    catch (error) { if (error.code !== 'ESRCH') throw error; }
  }

  // A tab of the client in this browser's profile (signed in once a device is), in `lang`.
  async newPage(lang = 'en') {
    const res = await fetch(`${this.devtools}/json/new?about:blank`, { method: 'PUT' });
    const target = await res.json();
    const p = await Page.open(target, this);
    p.targetId = target.id;
    this.pages.push(p);
    await p.send('Page.addScriptToEvaluateOnNewDocument', { source: startScript(lang) });
    await p.send('Page.navigate', { url: this.opts.url });
    // A tab navigated after it opened is not focused by itself, and full screen needs the focus.
    await p.send('Page.bringToFront');
    return p;
  }

  // The second tab of the first device (`h.a`), on the computer list. Made once, on first use.
  async secondPage() {
    if (this.b) return this.b;
    const b = await this.newPage();
    this.b = b;
    await b.addAuthenticator();
    await this.a.copyPasskeysTo(b);
    await b.until(`!!document.querySelector('.card')`, 'second browser device card');
    return b;
  }

  async closePage(p) {
    p.gone = true;
    await fetch(`${this.devtools}/json/close/${p.targetId}`);
  }

  // The browser itself, for browser contexts.
  async browserPage() {
    if (!this.root) {
      const version = await (await fetch(`${this.devtools}/json/version`)).json();
      this.root = await Page.open({ webSocketDebuggerUrl: version.webSocketDebuggerUrl }, this);
    }
    return this.root;
  }

  // A tab with its own cookies and storage, as if on another device. Not navigated yet;
  // `lang` null leaves the language to the browser's own choice.
  async contextPage({ lang = 'en' } = {}) {
    const root = await this.browserPage();
    const { browserContextId } = (await root.send('Target.createBrowserContext')).result;
    const { targetId } = (await root.send('Target.createTarget', { url: 'about:blank', browserContextId })).result;
    await sleep(300);
    const listed = await (await fetch(`${this.devtools}/json/list`)).json();
    const p = await Page.open(listed.find((x) => x.id === targetId), this);
    p.context = browserContextId;
    this.pages.push(p);
    await p.send('Page.addScriptToEvaluateOnNewDocument', { source: startScript(lang) });
    return p;
  }

  async dropContext(p) {
    p.gone = true;
    p.ws.close();
    await this.root.send('Target.disposeBrowserContext', { browserContextId: p.context });
  }

  // ---- the test computer ----

  // The pairing code an owner would read off the host's screen, in whichever language the host
  // speaks (its own setting, else the system's).
  pairCode() {
    const r = spawnSync(this.opts.hostExe, ['pair'], { env: { ...process.env, DESKWAY_HOME: this.opts.hostHome }, encoding: 'utf8', windowsHide: true });
    const m = PAIR_CODE.exec(r.stdout || '');
    if (!m) throw new Error(`no pairing code from the host: ${r.stdout} ${r.stderr}`);
    return m[1];
  }

  // The test host's panel pipe (dev mode names it after its config folder), used here the way
  // the person sitting at the computer uses the panel.
  panelPipe(home = this.opts.hostHome) {
    const hash = createHash('sha256').update(home).digest('hex').slice(0, 8);
    const sock = pipeConnect(`\\\\.\\pipe\\deskway-ui-dev-${hash}`);
    let error = null;
    sock.on('error', (err) => { error = err; });
    let buf = Buffer.alloc(0);
    const statuses = [];
    sock.on('data', (d) => {
      buf = Buffer.concat([buf, d]);
      while (buf.length >= 5 && buf.length >= 5 + buf.readUInt32LE(0)) {
        const len = buf.readUInt32LE(0);
        // Messages to a panel are `{"Status": {...}}` or `"Exit"` (service::net::ToPanel).
        if (buf[4] === 0) {
          const m = JSON.parse(buf.subarray(5, 5 + len).toString('utf8'));
          if (m && m.Status) statuses.push(m.Status);
        }
        buf = buf.subarray(5 + len);
      }
    });
    const send = (obj) => {
      const body = Buffer.from(JSON.stringify(obj));
      const head = Buffer.alloc(5);
      head.writeUInt32LE(body.length, 0);
      sock.write(Buffer.concat([head, body]));
    };
    return { statuses, send, get error() { return error; }, close: () => sock.destroy() };
  }

  // ---- the server ----

  // Stops the server listening on this run's URL (only that one: other runs may be going) and
  // starts it again from --server-exe with --server-state. The new one is stopped at the end.
  async restartServer() {
    const port = new URL(this.opts.url).port;
    const listening = spawnSync('netstat', ['-ano', '-p', 'TCP'], { encoding: 'utf8', windowsHide: true }).stdout
      .split('\n').map((l) => l.trim().split(/\s+/)).filter((f) => f[3] === 'LISTENING' && f[1].endsWith(`:${port}`)).map((f) => f[4]);
    for (const pid of new Set(listening)) spawnSync('taskkill', ['/F', '/PID', pid], { windowsHide: true });
    await sleep(500);
    this.server = spawn(this.opts.serverExe, ['-listen', `127.0.0.1:${port}`, '-data', this.opts.serverState], { stdio: 'ignore', windowsHide: true });
  }

  // ---- sessions ----

  // Ends every session open on this run's pages; with `watcher`, waits until the computer is
  // free as the server tells it.
  async freeComputer(watcher) {
    for (const p of this.pages.filter((x) => !x.gone)) await p.v('s && s.leave()').catch(() => {});
    if (watcher) await watcher.until(`d.devices()[0] && !d.devices()[0].controller`, 'the computer is free', 30000);
  }

  // A session on `p`, connected and with a picture: the one open, or a new one taking over.
  async sessionOn(p) {
    // In front, as a person would have it: background tabs run their timers and animations late.
    await p.send('Page.bringToFront');
    if (await p.v(`!!s && s.info().connection === 'connected'`)) return;
    for (const o of this.pages.filter((x) => !x.gone && x !== p)) await o.v('s && s.leave()').catch(() => {});
    await p.until(`d.devices().length > 0`, 'computers listed');
    await p.v(`d.open(d.devices()[0].id, { force: true })`);
    await p.until(`!!s && s.info().connection === 'connected' && s.info().w > 0 && s.info().input === 'open'`, 'session up', 30000);
  }
}
