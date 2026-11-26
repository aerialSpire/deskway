import { $app, h, svg, ICONS, loading } from '../dom.js';
import { t } from '../i18n.js';
import { api, signedOut } from '../net.js';
import { probeRelay } from '../relay-check.js';
import { state, setView } from '../state.js';
import { dialog, gotIt, nameDialog } from '../dialogs.js';
import { topbar, heading, updateNote, statusOf, copyableCode } from './common.js';
import { dropdown, menuSeparator } from './menu.js';
import { accountMenu } from './account.js';

let seq = 0;
const CHECK_BATCH = 3;

export async function renderRelays() {
  if (!state.me?.admin) { setView('list'); return; }
  const mine = ++seq;
  const current = () => mine === seq && state.view === 'relays';
  const results = h('div', { 'aria-live': 'polite' });
  const section = h('section', { hidden: true }, h('h2', { class: 'section-title' }, t('relays.checkTitle')), results);
  const check = h('button', { class: 'btn', onclick: async () => {
    check.disabled = true;
    try {
      await checkRelays(section, results, current);
    } finally {
      check.disabled = false;
    }
  } }, t('relays.check'));
  const page = (...body) => $app.replaceChildren(h('div', { class: 'page', 'data-page': 'relays' },
    topbar(accountMenu()), updateNote(), heading(t('relays.title'), { back: true }),
    h('div', { class: 'actions-row' },
      h('button', { class: 'btn primary', onclick: addRelay }, t('relays.add')),
      h('button', { class: 'btn', onclick: renderRelays }, t('relays.refresh')),
      check),
    ...body, section));
  page(loading(t('common.loading')));
  try {
    const res = await api('GET', '/api/relays');
    if (!res.ok) throw new Error(String(res.status));
    const list = await res.json();
    if (!current()) return;
    page(list.length ? h('div', { class: 'device-list' }, list.map(row)) : h('p', { class: 'hint' }, t('relays.empty')));
  } catch (e) {
    if (current() && !signedOut(e)) page(h('p', { class: 'hint' }, t('relays.loadFailed')));
  }
}

async function checkRelays(section, results, current) {
  section.hidden = false;
  results.replaceChildren(loading(t('relays.checking')));
  try {
    const res = await api('GET', '/api/relays/check');
    if (!res.ok) throw new Error(String(res.status));
    const relays = await res.json();
    if (!current()) return;
    const checks = [];
    const renderRows = relays.map(({ name, ice: { urls, username, credential } }) => {
      const stun = urls.find((url) => url.startsWith('stun:'));
      const address = new URL(stun.replace(':', '://'));
      const outcomes = [];
      const lines = urls.filter((url) => url !== stun).map((url) => {
        const endpoint = new URL(url.replace(':', '://'));
        const protocol = endpoint.searchParams.get('transport').toUpperCase();
        const transport = endpoint.protocol === 'turns:' ? 'TLS' : protocol;
        const line = h('div', { class: 'device-sub' });
        checks.push(async () => {
          const result = await probeRelay({ urls: url, username, credential }, stun, globalThis.RTCPeerConnection);
          outcomes.push(result);
          line.textContent = result === 'unreachable'
            ? t('relays.listener.unreachable', { transport, protocol, port: endpoint.port })
            : t('relays.listener.reachable', { transport });
        });
        return line;
      });
      return () => {
        const passed = outcomes.includes('connected');
        const traffic = passed || outcomes.includes('allocated')
          ? h('div', { class: 'device-sub' }, passed
            ? t('relays.traffic.passed')
            : t('relays.traffic.didNotPass', { port: address.port }))
          : null;
        return h('div', { class: 'device-row' }, h('div', { class: 'device-text' },
          h('div', { class: 'device-name' }, `${name ?? t('relays.main')} · ${address.host}`), lines, traffic));
      };
    });
    for (let i = 0; i < checks.length && current(); i += CHECK_BATCH) {
      await Promise.all(checks.slice(i, i + CHECK_BATCH).map((run) => run()));
    }
    if (current()) results.replaceChildren(renderRows.length
      ? h('div', { class: 'device-list' }, renderRows.map((render) => render()))
      : h('p', { class: 'hint' }, t('relays.checkEmpty')));
  } catch (e) {
    if (current() && !signedOut(e)) results.replaceChildren(h('p', { class: 'hint' }, t('relays.checkFailed')));
  }
}

function row(relay) {
  const more = h('button', { class: 'btn icon more', 'aria-label': t('relays.manage', { name: relay.name }) }, svg(ICONS.more));
  const menu = dropdown(`relay:${relay.id}`, more, (item) => [
    item(t('common.rename'), () => rename(relay)),
    menuSeparator(),
    item(t('common.remove'), () => remove(relay), { danger: true }),
  ]);
  return h('div', { class: 'device-row' },
    h('div', { class: 'device-icon' }, svg(ICONS.globe)),
    h('div', { class: 'device-text' },
      h('div', { class: 'device-name' }, h('span', { class: 'name', title: relay.name }, relay.name)),
      h('div', { class: 'device-sub' }, `${relay.public_ip}:${relay.port} · ${statusOf(relay).text}`)),
    h('div', { class: 'device-actions' }, menu.el));
}

async function addRelay(e) {
  if (location.protocol !== 'https:') {
    gotIt(t('relays.add'), t('relays.https'));
    return;
  }
  const button = e.currentTarget;
  button.disabled = true;
  try {
    const res = await api('POST', '/api/relays/token');
    if (!res.ok) throw new Error(String(res.status));
    const { token } = await res.json();
    if (state.view === 'relays') showCommand(`deskway-server relay -join ${location.origin} -token ${token}`);
  } catch (e) {
    if (state.view === 'relays' && !signedOut(e)) gotIt(t('relays.tokenFailed'), t('err.checkNetwork'));
  } finally {
    button.disabled = false;
  }
}

function showCommand(command, copyFailed = false) {
  dialog(t('relays.add'), h('div', {},
    h('p', {}, t('relays.joinNote')),
    copyableCode(command),
    copyFailed ? h('div', { class: 'error', role: 'alert' }, t('relays.copyFailed')) : null), [
    { label: t('common.close') },
    { label: t('relays.copy'), primary: true, run: async () => {
      try {
        await navigator.clipboard.writeText(command);
      } catch {
        if (state.view === 'relays') showCommand(command, true);
      }
    } },
  ]);
}

function rename(relay) {
  nameDialog(t('common.rename'), t('common.name'), relay.name, async (name) => {
    const res = await api('PATCH', `/api/relays/${relay.id}`, { name });
    if (res.ok && state.view === 'relays') renderRelays();
    return res.ok;
  });
}

function remove(relay) {
  dialog(t('relays.removeTitle', { name: relay.name }), t('relays.removeText'), [
    { label: t('common.cancel') },
    { label: t('common.remove'), primary: true, run: async () => {
      try {
        const res = await api('DELETE', `/api/relays/${relay.id}`);
        if (!res.ok) throw new Error(String(res.status));
        if (state.view === 'relays') renderRelays();
      } catch (e) {
        if (state.view === 'relays' && !signedOut(e)) gotIt(t('relays.notRemoved'), t('err.checkNetwork'));
      }
    } },
  ]);
}
