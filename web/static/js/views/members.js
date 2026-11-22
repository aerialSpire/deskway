// Members: the admin's members (docs/accounts.md), opened from the menu under the admin's name.
// The list (names, when made, when last active, how many computers), Create user, and under each
// member's ⋯ resetting its password and removing it. A new or reset password is shown once, in
// a dialog whose one “Copy” copies the message to send the member.
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

function row(m) {
  const sub = [
    t('account.signsInAs', { name: m.name }),
    t('members.created', { date: dateText(m.created) }),
    m.last_active ? agoText(m.last_active) : t('members.neverActive'),
    t('members.computers', { n: m.computers }),
  ].join(' · ');
  const button = h('button', { class: 'btn icon more', 'aria-label': t('members.menu', { name: m.display }) }, svg(ICONS.more));
  const menu = dropdown(`member:${m.id}`, button, (item) => [
    item(t('members.reset'), () => confirmReset(m)),
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
    return () => showPassword(t('members.createdTitle'), made.name, made.password);
  });
}

function confirmReset(m) {
  dialog(t('members.resetTitle', { name: m.display }), t('members.resetText', { name: m.display }), [
    { label: t('common.cancel') },
    { label: t('members.resetGo'), primary: true, run: async () => {
      let done;
      try {
        const res = await api('POST', `/api/members/${m.id}/password`);
        if (!res.ok) throw new Error(String(res.status));
        done = await res.json();
      } catch (e) {
        if (!signedOut(e)) gotIt(t('members.notReset'), t('err.checkNetwork'));
        return;
      }
      showPassword(t('members.resetDoneTitle'), done.name, done.password);
    } },
  ]);
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

// The message the admin sends the member, in the admin's language: where, who, the password.
const passwordMessage = (name, password) => t('members.message', { link: `${location.origin}/`, name, password });

// A new initial password, shown this once: the link, the account and the password, and the one
// button that copies them as one message. If the browser does not let the page copy, the
// dialog comes back saying so, to be written down instead.
function showPassword(title, name, password, copyFailed = false) {
  const line = (label, value) => h('div', { class: 'setting-row' }, h('div', { class: 'device-sub' }, label), h('div', { class: 'credential' }, value));
  const body = h('div', {},
    h('p', {}, t('members.onceText')),
    h('div', { class: 'credentials' },
      line(t('members.link'), `${location.origin}/`),
      line(t('members.account'), name),
      line(t('members.password'), password)),
    copyFailed ? h('div', { class: 'error', role: 'alert' }, t('members.copyFailed')) : null);
  dialog(title, body, [
    { label: t('common.close') },
    { label: t('members.copy'), primary: true, run: () => {
      navigator.clipboard.writeText(passwordMessage(name, password))
        .then(() => gotIt(t('members.copiedTitle'), t('members.copiedText')))
        .catch(() => showPassword(title, name, password, true));
    } },
  ]);
}
