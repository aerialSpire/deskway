// AI activity on a computer's page, below its screens: the calls the computer recorded of AI
// clients, a page at a time, narrowed by time, AI client, tool and failures (docs/security.md,
// "AI clients at the computer"). The computer answers each page through the server.
//
// One scroll, the page's: a call is one line (a card on a narrow screen), and opens in place
// to everything recorded of it. On a wide screen the title, the filters and the columns' names
// stay at the top of the window while the calls scroll under them, in one row once stuck.
//
// Closed on entering the page: a card with only its title, asking the computer for nothing.
// Pressing the title opens it and asks; pressing the title again closes it (state.auditOpen).
//
// Beside the title: whether AI may control the computer, with the way to add an AI client (or
// to My devices to allow it). A computer too old for AI control says so instead.
//
// Public surface: auditSection(d), onAudit(m).
import { h, svg, ICONS } from '../dom.js';
import { t } from '../i18n.js';
import { socket } from '../net.js';
import { Signal } from '../proto.js';
import { state, device, render, setView } from '../state.js';
import { aiSupported } from './common.js';
import { openAiClient } from './ai-client.js';
import { PER_PAGE, RANGES, auditQuery } from './audit-query.js';

// The first page is asked for again this often while it is shown: new calls appear.
const EVERY = 15000;

// Per computer: what is chosen, the page last answered, what was last asked, the calls open.
const kept = {};
const of = (id) => kept[id] || (kept[id] = { choice: { range: 'all', from: '', to: '', client: '', tool: '', failed: false, page: 0 }, page: null, asked: '', at: 0, open: new Set() });

function ask(d, force = false) {
  const k = of(d.id);
  const query = auditQuery(k.choice);
  const key = JSON.stringify(query);
  if (!socket.isOpen() || (!force && key === k.asked && Date.now() - k.at < EVERY)) return;
  k.asked = key;
  k.at = Date.now();
  socket.sendNow({ type: Signal.AUDIT_REQ, device: d.id, query });
}

export function onAudit(m) {
  of(m.device).page = m.page;
  if (state.view === 'detail' && state.deviceId === m.device) render();
}

setInterval(() => {
  const d = state.view === 'detail' ? device() : null;
  if (d && state.auditOpen && d.online && keeps(d) && of(d.id).choice.page === 0) ask(d);
}, EVERY);

// A computer that keeps such a record (one too old keeps none).
const keeps = (d) => Array.isArray(d.tools) && d.tools.includes('audit_read');

// Back to the top of the section, where the filters are (after choosing, or another page).
const toTop = () => requestAnimationFrame(() => document.querySelector('.audit')?.scrollIntoView({ block: 'start' }));

// A change of what is chosen: from the first page again, asked at once.
function choose(d, change) {
  const k = of(d.id);
  Object.assign(k.choice, { page: 0 }, change);
  k.open.clear();
  ask(d, true);
  render();
}

const pad = (n) => String(n).padStart(2, '0');
// The time a call started, to the second; the date too when it was not today.
const when = (at) => {
  const date = new Date(at * 1000);
  const time = `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
  return new Date().toDateString() === date.toDateString() ? time : `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${time}`;
};
const ok = (r) => r === 'ok' || r === 'running';
// The outcome in words: done, running, or why not (the error's code is in the details).
const outcome = (r) => {
  if (r === 'ok') return t('audit.ok');
  if (r === 'running') return t('audit.running');
  const words = t(`audit.code.${r}`);
  return words === `audit.code.${r}` ? t('audit.code.failed') : words;
};
const took = (ms) => (ms == null ? '' : ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`);
const keyOf = (c) => `${c.at}|${c.session}|${c.tool}|${c.args}`;

function select(label, value, options, onChange) {
  const el = h('select', { class: 'input', 'aria-label': label }, ...options.map(([v, text]) => h('option', { value: v, selected: v === value }, text)));
  el.addEventListener('change', () => onChange(el.value));
  return el;
}

function filters(d, k) {
  const c = k.choice;
  const known = k.page || { clients: [], tools: [] };
  const date = (value, label, key) => {
    const el = h('input', { class: 'input', type: 'date', value, 'aria-label': label });
    el.addEventListener('change', () => choose(d, { [key]: el.value }));
    return el;
  };
  const failed = h('input', { type: 'checkbox', checked: c.failed });
  failed.addEventListener('change', () => choose(d, { failed: failed.checked }));
  return h('div', { class: 'audit-filters' },
    select(t('audit.range'), c.range, RANGES.map((r) => [r, t(`audit.range.${r}`)]), (range) => choose(d, { range })),
    c.range === 'custom' ? date(c.from, t('audit.from'), 'from') : null,
    c.range === 'custom' ? date(c.to, t('audit.to'), 'to') : null,
    select(t('audit.client'), c.client, [['', t('audit.allClients')], ...known.clients.map((x) => [x, x])], (client) => choose(d, { client })),
    select(t('audit.tool'), c.tool, [['', t('audit.allTools')], ...known.tools.map((x) => [x, x])], (tool) => choose(d, { tool })),
    h('label', { class: 'audit-filter' }, failed, t('audit.failedOnly')));
}

// One call: its line (a card on a narrow screen), opening in place to all that was recorded.
function call(k, c) {
  const key = keyOf(c);
  const open = k.open.has(key);
  const line = h('button', { class: 'audit-line', type: 'button', 'aria-expanded': String(open), onclick: () => { if (open) k.open.delete(key); else k.open.add(key); render(); } },
    h('span', { class: 'col-when' }, when(c.at)),
    h('span', { class: 'col-client' }, c.client),
    h('span', { class: 'col-tool mono' }, c.tool),
    h('span', { class: 'col-args' }, c.args),
    h('span', { class: `col-result ${ok(c.result) ? '' : 'failed'}` }, outcome(c.result)),
    h('span', { class: 'col-took' }, took(c.ms)));
  if (!open) return h('div', { class: 'audit-call' }, line);
  const item = (label, value, cls = '') => (value === '' || value == null ? null : [h('dt', {}, label), h('dd', { class: cls }, value)]);
  return h('div', { class: 'audit-call open' }, line,
    h('dl', { class: 'audit-detail' },
      item(t('audit.when'), when(c.at)),
      item(t('audit.client'), c.client),
      item(t('audit.tool'), c.tool, 'mono'),
      item(t('audit.args'), c.args, 'mono wrap'),
      item(t('audit.result'), ok(c.result) ? outcome(c.result) : `${outcome(c.result)} (${c.result})`, ok(c.result) ? '' : 'failed'),
      item(t('audit.detail'), c.detail, 'wrap'),
      item(t('audit.took'), took(c.ms)),
      item(t('audit.session'), c.session, 'mono')));
}

function pager(d, k) {
  const pages = Math.max(1, Math.ceil(k.page.total / PER_PAGE));
  const at = k.choice.page;
  const go = (p) => { k.choice.page = p; k.open.clear(); ask(d, true); render(); toTop(); };
  return h('div', { class: 'audit-pager' },
    h('span', { class: 'audit-total' }, t('audit.total', { n: k.page.total })),
    h('div', { class: 'audit-pages' },
      h('button', { class: 'btn compact', type: 'button', disabled: at === 0, onclick: () => go(at - 1) }, t('audit.prev')),
      h('span', {}, t('audit.pageOf', { page: at + 1, pages })),
      h('button', { class: 'btn compact', type: 'button', disabled: at + 1 >= pages, onclick: () => go(at + 1) }, t('audit.next'))));
}

// Whether the head is stuck at the top of the window: a mark just above it leaves the window.
const stuck = new IntersectionObserver((seen) => {
  for (const e of seen) e.target.nextElementSibling?.classList.toggle('stuck', !e.isIntersecting && e.boundingClientRect.top < 0);
});

// Whether AI may control the computer, and what to do next.
function aiState(d) {
  const st = d.ai && d.ai.state;
  const link = (key, run) => h('button', { class: 'btn text compact', type: 'button', onclick: run }, t(key));
  if (st === 'on') return h('div', { class: 'audit-ai' }, h('span', { class: 'ok' }, t('audit.allowed')), link('audit.addClient', openAiClient));
  return h('div', { class: 'audit-ai' }, h('span', {}, t(st === 'stopped' ? 'audit.stopped' : 'audit.notAllowed')), link('audit.turnOn', () => setView('devices')));
}

// The title, pressed to open or close the record. Closing from far down the record leaves the
// closed card where the window shows it.
function toggle() {
  const open = state.auditOpen;
  return h('h2', {}, h('button', { class: 'audit-toggle', type: 'button', 'aria-expanded': String(open), onclick: () => {
    state.auditOpen = !open;
    render();
    if (open && document.querySelector('.audit')?.getBoundingClientRect().top < 0) toTop();
  } }, t('audit.title'), svg(ICONS.chevron)));
}

export function auditSection(d) {
  if (!aiSupported(d)) return h('section', { class: 'audit', 'data-audit': d.id }, h('p', { class: 'audit-note' }, t('devices.aiOld')));
  if (!state.auditOpen) return h('section', { class: 'audit closed', 'data-audit': d.id }, h('div', { class: 'audit-title' }, toggle(), aiState(d)));
  const k = of(d.id);
  if (d.online && keeps(d)) ask(d);
  const listed = k.page && k.page.calls.length;
  const columns = listed ? h('div', { class: 'audit-line columns', 'aria-hidden': 'true' }, ...['when', 'client', 'tool', 'args', 'result', 'took'].map((x) => h('span', { class: `col-${x}` }, t(`audit.${x}`)))) : null;
  const body = !k.page ? h('p', { class: 'audit-note' }, t(d.online ? 'audit.loading' : 'audit.offline'))
    : !listed ? h('p', { class: 'audit-note' }, t('audit.none'))
    : [h('div', { class: 'audit-list' }, ...k.page.calls.map((c) => call(k, c))), pager(d, k)];
  const mark = h('div', { class: 'audit-mark', 'aria-hidden': 'true' });
  stuck.observe(mark);
  return h('section', { class: 'audit', 'data-audit': d.id }, mark,
    h('div', { class: 'audit-head' }, h('div', { class: 'audit-bar' }, h('div', { class: 'audit-title' }, toggle(), aiState(d)), keeps(d) ? filters(d, k) : null), columns),
    keeps(d) ? body : null);
}
