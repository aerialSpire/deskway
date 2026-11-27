// Members and their account links. A new link is shown once with a message to copy.
import { $app, h, svg, ICONS, loading } from '../dom.js';
import { t, agoText, dateText } from '../i18n.js';
import { api, signedOut } from '../net.js';
import { state, setView } from '../state.js';
import { dialog, formDialog, gotIt, shownError } from '../dialogs.js';
import { topbar, heading, updateNote } from './common.js';
import { dropdown, menuSeparator } from './menu.js';
import { accountMenu } from './account.js';

let seq = 0;

export async function renderMembers() {
  // Only the admin has this page; anyone else is taken back to the computers.
  if (!state.me?.admin) { setView('list'); return; }
  const mine = ++seq;
  const page = (...body) => $app.replaceChildren(h('div', { class: 'page', 'data-page': 'members' },
    topbar(accountMenu()), updateNote(), heading(t('members.title'), { back: true }), ...body));
  if (!$app.querySelector('.page[data-page="members"]')) page(loading(t('common.loading')));
  let list;
  try {
    const res = await api('GET', '/api/members');
    if (!res.ok) throw new Error(String(res.status));
    list = await res.json();
  } catch (e) {
    if (mine !== seq || signedOut(e)) return;
    page(h('p', { class: 'hint' }, t('members.loadFailed')));
    return;
  }
  if (mine !== seq || state.view !== 'members') return;
  page(
    h('p', { class: 'lead' }, t('members.intro')),
    h('div', { class: 'actions-row' }, h('button', { class: 'btn primary', onclick: showCreate }, t('members.create'))),
    list.length ? h('div', { class: 'device-list' }, list.map(row)) : h('p', { class: 'hint members-empty' }, t('members.empty')));
}

function linkStatus(m) {
  if (!m.link_expiry) return m.needs_setup ? t('members.needsLink') : '';
  const days = Math.ceil((m.link_expiry - Date.now() / 1000) / 86400);
  if (!m.link_kind || days <= 0) return t('members.expired');
  const kind = m.link_kind === 'reset' ? t('members.unusedReset') : t('members.unusedInvite');
  return [kind, t('members.expiresIn', { n: days })].join(' · ');
}

function row(m) {
  const sub = [
    t('account.signsInAs', { name: m.name }),
    t('members.created', { date: dateText(m.created) }),
    m.last_active ? agoText(m.last_active) : t('members.neverActive'),
    t('members.computers', { n: m.computers }),
    linkStatus(m),
  ].filter(Boolean).join(' · ');
  const button = h('button', { class: 'btn icon more', 'aria-label': t('members.menu', { name: m.display }) }, svg(ICONS.more));
  const menu = dropdown(`member:${m.id}`, button, (item) => [
    item(t('members.newLink'), () => updateLink(m, 'POST')),
    m.link_kind && item(t('members.revokeLink'), () => updateLink(m, 'DELETE')),
    menuSeparator(),
    item(t('members.remove'), () => confirmRemove(m), { danger: true }),
  ]);
  return h('div', { class: 'device-row', 'data-member': m.name },
    h('div', { class: 'device-icon' }, svg(ICONS.person)),
    h('div', { class: 'device-text' },
      h('div', { class: 'device-name' }, h('span', {}, m.display)),
      h('div', { class: 'device-sub' }, sub)),
    h('div', { class: 'device-actions' }, menu.el));
}

function showCreate() {
  const name = h('input', { class: 'input', maxlength: 32, autocomplete: 'off', autocapitalize: 'none', spellcheck: 'false', required: true });
  const display = h('input', { class: 'input', maxlength: 20, autocomplete: 'off' });
  formDialog(t('members.createTitle'), t('members.createIntro'), [
    h('label', {}, t('members.name'), name),
    h('div', { class: 'hint' }, t('signin.rule')),
    h('label', {}, t('members.display'), display),
  ], t('members.createGo'), async () => {
    const value = name.value.trim();
    if (!/^[A-Za-z0-9._-]{2,32}$/.test(value)) throw shownError('signin.rule');
    const res = await api('POST', '/api/members', { name: value, display: display.value.trim() });
    if (res.status === 409) throw shownError('signin.taken');
    if (!res.ok) throw shownError('err.notSaved');
    const made = await res.json();
    renderMembers();
    return () => showLink(t('members.createdTitle'), made.name, made.link);
  });
}

async function updateLink(m, method) {
  try {
    const res = await api(method, '/api/members/' + m.id + '/link');
    if (!res.ok) throw new Error(String(res.status));
    if (method === 'POST') {
      const { name, link } = await res.json();
      showLink(t('members.newLink'), name, link);
    }
    renderMembers();
  } catch (e) {
    if (!signedOut(e)) gotIt(t('err.notSaved'), t('err.checkNetwork'));
  }
}

function confirmRemove(m) {
  dialog(t('members.removeTitle', { name: m.display }), t('members.removeText', { n: m.computers }), [
    { label: t('common.cancel') },
    { label: t('common.remove'), primary: true, run: async () => {
      try {
        const res = await api('DELETE', `/api/members/${m.id}`);
        if (!res.ok) throw new Error(String(res.status));
      } catch (e) {
        if (!signedOut(e)) gotIt(t('members.notRemoved'), t('err.checkNetwork'));
        return;
      }
      renderMembers();
    } },
  ]);
}

function showLink(title, name, path, copyFailed = false) {
  const link = new URL(path, location.origin).href;
  const line = (label, value) => h('div', { class: 'setting-row' }, h('div', { class: 'device-sub' }, label), h('div', { class: 'credential' }, value));
  const body = h('div', {},
    h('p', {}, t('members.onceText')),
    h('div', { class: 'credentials' },
      line(t('members.link'), link),
      line(t('members.account'), name)),
    copyFailed ? h('div', { class: 'error', role: 'alert' }, t('members.copyFailed')) : null);
  dialog(title, body, [
    { label: t('common.close') },
    { label: t('members.copy'), primary: true, run: () => {
      navigator.clipboard.writeText(t('members.message', { link, name }))
        .then(() => gotIt(t('members.copiedTitle'), t('members.copiedText')))
        .catch(() => showLink(title, name, path, true));
    } },
  ]);
}
