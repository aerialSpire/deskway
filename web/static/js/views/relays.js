import { $app, h, svg, ICONS, loading } from '../dom.js';
import { t } from '../i18n.js';
import { api, signedOut } from '../net.js';
import { state, setView } from '../state.js';
import { dialog, gotIt, nameDialog } from '../dialogs.js';
import { topbar, heading, updateNote, statusOf, copyableCode } from './common.js';
import { dropdown, menuSeparator } from './menu.js';
import { accountMenu } from './account.js';

let seq = 0;

export async function renderRelays() {
  if (!state.me?.admin) { setView('list'); return; }
  const mine = ++seq;
  const current = () => mine === seq && state.view === 'relays';
  const page = (...body) => $app.replaceChildren(h('div', { class: 'page', 'data-page': 'relays' },
    topbar(accountMenu()), updateNote(), heading(t('relays.title'), { back: true }),
    h('div', { class: 'actions-row' },
      h('button', { class: 'btn primary', onclick: addRelay }, t('relays.add')),
      h('button', { class: 'btn', onclick: renderRelays }, t('relays.refresh'))),
    ...body));
  if (!$app.querySelector('.page[data-page="relays"]')) page(loading(t('common.loading')));
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
