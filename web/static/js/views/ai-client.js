// Add an AI client: connects an AI client (Claude Code, Cursor and others that speak MCP) to the
// account without the account password (docs/architecture.md, "Identity"). Three steps: the
// prompt to send the AI, with the program for each system and its SHA-256 as the server has
// them; approving the AI here when it signs in, with the digits it tells; and the computers it
// may control. Copying the prompt opens the account to new AI sign-ins for 10 minutes: the
// server refuses them at any other time. An AI client's request is answered here while this
// page is open, not in a dialog (approvals.js).
//
// Public surface: openAiClient(), renderAiClient().
import { $app, h } from '../dom.js';
import { t } from '../i18n.js';
import { api, signedOut } from '../net.js';
import { keyCode } from '../auth-messages.js';
import { state, setView, render } from '../state.js';
import { topbar, heading, updateNote, aiSupported, codeBox } from './common.js';
import { accountMenu } from './account.js';
import { deviceOf, answerDevice } from './approvals.js';

// Where the program goes on the AI's computer: its user folder, no administrator rights.
const PATH = { windows: '%LOCALAPPDATA%\\Deskway\\bin\\deskway.exe', unix: '~/.deskway/bin/deskway' };

const page = { from: 'devices', files: null, until: 0, copied: false, copyFailed: false, open: false, manual: false, codes: {} };
let timer = 0;

// Opened from My devices or a computer's page; the way back returns there.
export function openAiClient() {
  Object.assign(page, { from: state.view === 'detail' ? 'detail' : 'devices', files: null, copied: false, copyFailed: false, open: false, manual: false });
  setView('aiclient');
  load();
}

async function load() {
  try {
    const [files, win] = await Promise.all([api('GET', '/api/ai/clients'), api('GET', '/api/ai/window')]);
    page.files = files.ok ? await files.json() : [];
    if (win.ok) page.until = (await win.json()).until;
  } catch (e) {
    if (signedOut(e)) return;
    page.files = [];
  }
  if (state.view === 'aiclient') render();
}

const now = () => Math.floor(Date.now() / 1000);
const label = (f) => `${t(`aiclient.os.${f.os}`)} ${f.os === 'macos' ? t(`aiclient.mac.${f.arch}`) : f.arch === 'x64' ? 'x64' : 'ARM64'}`;
const url = (f) => `${location.origin}/download/${f.file}`;

// The prompt, in the page's language: the server, the account, and each program with its hash.
function prompt() {
  const files = page.files.map((f) => t('aiclient.promptFile', { label: label(f), url: url(f), sha: f.sha256 })).join('\n');
  return t('aiclient.prompt', { server: location.origin, user: state.me.user, files, windows: PATH.windows, unix: PATH.unix });
}

async function copy() {
  page.copyFailed = false;
  try {
    await navigator.clipboard.writeText(prompt());
    page.copied = true;
  } catch {
    page.copyFailed = true;
    page.open = true;
  }
  // Copied or not (it may be copied by hand), the account takes AI sign-ins for 10 minutes now.
  try {
    const res = await api('POST', '/api/ai/window');
    if (res.ok) page.until = (await res.json()).until;
  } catch (e) {
    if (signedOut(e)) return;
  }
  render();
}

function stepPrompt() {
  if (!page.files) return h('p', { class: 'hint' }, t('common.loading'));
  if (!page.files.length) return h('p', { class: 'hint ai-note' }, t('aiclient.noFiles'));
  return h('div', { class: `ai-prompt ${page.open ? 'open' : ''}` },
    h('pre', { class: 'ai-prompt-text' }, prompt()),
    h('div', { class: 'ai-prompt-bar' },
      h('button', { class: 'btn text compact', type: 'button', 'aria-expanded': String(page.open), onclick: () => { page.open = !page.open; render(); } }, t(page.open ? 'aiclient.less' : 'aiclient.more')),
      h('button', { class: 'btn primary compact', type: 'button', onclick: copy }, t(page.copied ? 'aiclient.copied' : 'aiclient.copy'))),
    page.copyFailed ? h('div', { class: 'error', role: 'alert' }, t('aiclient.copyFailed')) : null);
}

// The newest AI client waiting, with its digits beside Deny and Approve.
function request() {
  const a = [...state.approvals].reverse().find((x) => deviceOf(x)?.role === 'ai');
  if (!a) return null;
  const dev = deviceOf(a);
  if (!page.codes[a.id]) keyCode(dev.key, dev.role).then((code) => { page.codes[a.id] = code; if (state.view === 'aiclient') render(); });
  const answer = (allow) => async () => { await answerDevice(a, dev, allow); render(); };
  return h('div', { class: 'ai-request', 'data-approval': a.id },
    h('div', {}, t('aiclient.request', { name: dev.name })),
    page.codes[a.id] ? codeBox(page.codes[a.id], page.codes[a.id]) : null,
    h('div', { class: 'ai-request-bar' },
      h('span', { class: 'hint' }, t('aiclient.match')),
      h('button', { class: 'btn compact', type: 'button', onclick: answer(false) }, t('common.deny')),
      h('button', { class: 'btn primary compact', type: 'button', onclick: answer(true) }, t('common.approve'))));
}

const left = () => {
  const s = Math.max(0, page.until - now());
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
};

function stepApprove() {
  const asked = request();
  if (asked) return asked;
  if (page.until > now()) {
    return h('div', {},
      h('div', { class: 'ai-wait', role: 'status' }, h('span', { class: 'spinner', 'aria-hidden': 'true' }), h('span', { class: 'ai-left' }, t('aiclient.waiting', { time: left() }))),
      h('p', { class: 'hint' }, t('aiclient.waitNote')));
  }
  return h('p', { class: 'hint' }, t(page.copied ? 'aiclient.ended' : 'aiclient.idle'));
}

// Each computer: whether AI may control it, and the way to My devices to change that.
function stepComputers() {
  if (!state.devices.length) return h('p', { class: 'hint' }, t('aiclient.none'));
  const turnOn = () => h('button', { class: 'btn text compact', type: 'button', onclick: () => setView('devices') }, t('aiclient.turnOn'));
  return h('div', { class: 'ai-computers' }, state.devices.map((d) => {
    const ai = d.ai && d.ai.state;
    const status = !aiSupported(d) ? h('span', { class: 'ai-state' }, t('aiclient.old'))
      : ai === 'on' ? h('span', { class: 'ai-state ok' }, t('aiclient.on'))
      : h('span', { class: 'ai-state' }, t(ai === 'stopped' ? 'aiclient.stopped' : 'aiclient.off'), ' · ', turnOn());
    return h('div', { class: 'ai-computer' }, h('span', { class: 'name', title: d.name }, d.name), status);
  }));
}

function manual() {
  if (!page.files || !page.files.length) return null;
  const code = (text) => h('code', { class: 'ai-code' }, text);
  const path = `<${t('aiclient.path')}>`;
  return h('details', { class: 'ai-manual', open: page.manual, ontoggle: (e) => { page.manual = e.target.open; } },
    h('summary', {}, t('aiclient.manual')),
    h('div', { class: 'ai-manual-body' },
      h('h3', {}, t('aiclient.mDownload')),
      ...page.files.map((f) => code(`${label(f)}\n${url(f)}\nSHA-256 ${f.sha256}`)),
      h('h3', {}, t('aiclient.mLogin')),
      code(`${path} login --server ${location.origin} --user ${state.me.user} --name "Claude Code"`),
      h('h3', {}, t('aiclient.mClaude')),
      code(`claude mcp add deskway -- ${path} mcp`),
      h('h3', {}, t('aiclient.mOther')),
      code(JSON.stringify({ mcpServers: { deskway: { command: path, args: ['mcp'] } } }, null, 2))));
}

export function renderAiClient() {
  if (!state.me) return;
  clearInterval(timer);
  const step = (title, sub, body) => h('li', { class: 'ai-step' }, h('h2', {}, title), h('p', { class: 'hint' }, sub), body);
  $app.replaceChildren(h('div', { class: 'page', 'data-page': 'aiclient' },
    topbar(accountMenu()), updateNote(),
    heading(t('aiclient.title'), { back: page.from }),
    h('p', { class: 'hint ai-lead' }, t('aiclient.lead')),
    h('ol', { class: 'ai-steps' },
      step(t('aiclient.s1'), t('aiclient.s1Sub'), stepPrompt()),
      step(t('aiclient.s2'), t('aiclient.s2Sub'), stepApprove()),
      step(t('aiclient.s3'), t('aiclient.s3Sub'), stepComputers())),
    manual()));
  // The time left, each second; at its end the step says the window has closed.
  if (page.until > now()) {
    timer = setInterval(() => {
      if (state.view !== 'aiclient') { clearInterval(timer); return; }
      if (page.until <= now()) { render(); return; }
      const el = $app.querySelector('.ai-left');
      if (el) el.textContent = t('aiclient.waiting', { time: left() });
    }, 1000);
  }
}
