// The account: its menu under the name in the top bar, and its page (Account) where the display
// name, the sign-in name and password are changed, each in the same kind of dialog.
import { $app, h, svg, ICONS } from '../dom.js';
import { standalone } from '../env.js';
import { t } from '../i18n.js';
import { api } from '../net.js';
import { state, setView, render } from '../state.js';
import { formDialog, gotIt, shownError } from '../dialogs.js';
import { canPromptInstall, promptInstall } from '../pwa.js';
import { showLogin } from './login.js';
import { dropdown, menuSeparator } from './menu.js';
import { topbar, heading, updateNote, settingRow } from './common.js';

// The name in the top bar opens the account's places: its page, its devices, and for the admin
// Members and Relays.
export function accountMenu() {
  const button = h('button', { class: 'btn text bar-btn name-btn' },
    h('span', { class: 'bar-name' }, state.me.display), svg(ICONS.chevron));
  return dropdown('account', button, (item) => {
    const install = installItem(item);
    return [
      h('div', { class: 'menu-head' }, state.me.display, h('span', {}, t('account.signsInAs', { name: state.me.user }))),
      item(t('account.title'), () => setView('account')),
      item(t('devices.title'), () => setView('devices')),
      state.me.admin && item(t('members.title'), () => setView('members')),
      state.me.admin && item(t('relays.title'), () => setView('relays')),
      install && menuSeparator(), install,
      menuSeparator(),
      item(t('menu.signOut'), async () => { await api('POST', '/api/logout').catch(() => {}); showLogin(); }),
    ];
  }).el;
}

// Installing Deskway, decided by what the browser offers, never by which device it is
// (docs/design.md): the browser's own install prompt where there is one; Safari's way, which
// only it has (navigator.standalone exists there), explained; nothing once installed.
function installItem(item) {
  if (standalone()) return null;
  if (canPromptInstall()) return item(t('menu.install'), promptInstall);
  if ('standalone' in navigator) return item(t('menu.addToHome'), showHomeScreenSteps);
  return null;
}

function showHomeScreenSteps() {
  const steps = h('ol', { class: 'steps' },
    h('li', {}, t('home.step1')), h('li', {}, t('home.step2')), h('li', {}, t('home.step3')));
  const note = h('div', { class: 'note-box' }, h('div', {}, h('b', {}, t('home.newDeviceTitle')), t('home.newDeviceText')));
  gotIt(t('home.title'), h('div', {}, h('p', {}, t('home.text')), steps, note), { primary: true });
}

// Account: one row per thing the account has, each changed in its own dialog.
export function renderAccount() {
  const row = (label, value, change) => settingRow(label, value, h('button', { class: 'btn compact', onclick: change }, t('common.change')));
  $app.replaceChildren(h('div', { class: 'page', 'data-page': 'account' },
    topbar(accountMenu()),
    updateNote(),
    heading(t('account.title'), { back: true }),
    h('div', { class: 'setting-list' },
      row(t('account.display'), state.me.display, showChangeDisplay),
      row(t('account.name'), state.me.user, showChangeName),
      row(t('account.password'), t('account.passwordSub'), showChangePassword))));
}

// The account as the server now has it: the top bar and the page show it at once.
async function took(res) {
  state.me = await res.json();
  render();
}

function showChangeDisplay() {
  const input = h('input', { class: 'input', value: state.me.display, maxlength: 20, placeholder: t('account.display'), 'aria-label': t('account.display') });
  formDialog(t('display.title'), t('display.intro'), [input], t('common.save'), async () => {
    const name = input.value.trim();
    if (!name) throw shownError('display.empty');
    if (name === state.me.display) return null;
    const res = await api('PATCH', '/api/me', { display: name });
    if (!res.ok) throw shownError('err.notSaved');
    await took(res);
    return null;
  });
  input.select();
}

function showChangeName() {
  const name = h('input', { class: 'input', value: state.me.user, maxlength: 32, autocomplete: 'username', autocapitalize: 'none', spellcheck: 'false', placeholder: t('signin.new'), 'aria-label': t('signin.new') });
  const password = secretInput(t('pw.current'), 'current-password');
  formDialog(t('signin.title'), t('signin.intro'), [name, h('div', { class: 'hint' }, t('signin.rule')), password], t('common.save'), async () => {
    const value = name.value.trim();
    if (!/^[A-Za-z0-9._-]{2,32}$/.test(value)) throw shownError('signin.rule');
    const res = await api('POST', '/api/me/name', { name: value, password: password.value });
    if (res.status === 409) throw shownError('signin.taken');
    await secretResult(res, 'pw.wrongCurrent');
    await took(res);
    return { title: t('signin.doneTitle'), text: t('signin.doneText', { name: value }) };
  });
  name.select();
}

const secretInput = (label, autocomplete) => h('input', { class: 'input', type: 'password', required: true, autocomplete, placeholder: label, 'aria-label': label });

// Turns a server answer into the message the person should see.
async function secretResult(res, wrong) {
  if (res.ok) return true;
  if (res.status === 403) throw shownError(wrong);
  if (res.status === 429) throw shownError('err.tooMany');
  throw shownError('err.notSaved');
}

function showChangePassword() {
  const current = secretInput(t('pw.current'), 'current-password');
  const next = secretInput(t('pw.new'), 'new-password');
  const again = secretInput(t('pw.again'), 'new-password');
  const others = h('input', { type: 'checkbox' });
  formDialog(t('pw.title'), t('pw.intro'),
    [current, next, again, h('label', { class: 'check' }, others, t('pw.signOutOthers'))], t('common.save'), async () => {
      if (next.value.length < 8) throw shownError('pw.tooShort');
      if (new TextEncoder().encode(next.value).length > 72) throw shownError('pw.tooLong');
      if (next.value !== again.value) throw shownError('pw.mismatch');
      await secretResult(await api('POST', '/api/password', { current: current.value, new: next.value, sign_out_others: others.checked }), 'pw.wrongCurrent');
      return { title: t('pw.doneTitle'), text: others.checked ? t('pw.doneOthers') : t('pw.doneNext') };
    });
}
