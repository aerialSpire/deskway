// Signing in: the account password, then (for a new device) approval on a device already
// signed in, or the super password; with an initial password, choosing the account's own
// (docs/accounts.md, First sign-in). Also the start of the page: who is signed in.
import { $app, h, loading } from '../dom.js';
import { separateHomeScreenApp } from '../env.js';
import { t, currentLang, setLanguage } from '../i18n.js';
import { api, socket, signedOut } from '../net.js';
import { ensurePasskey, storedPasskey, deviceLabel } from '../auth.js';
import { keyCode } from '../auth-messages.js';
import { state, setView, currentSession } from '../state.js';
import { closeDialogs } from '../dialogs.js';
import { ownComputer } from '../windows.js';
import { languagePicker, topbar, heading, codeBox } from './common.js';

// Finds out who is signed in, then shows the computers, or the sign-in page.
export async function start() {
  let res;
  try {
    // Not through api(): a 401 here is the answer asked for, not a sign-out.
    res = await fetch('/api/me');
  } catch {
    // Not signed out: the server is unreachable. Saying "sign in" here would be wrong.
    showUnreachable();
    return;
  }
  if (res.status === 401) { showLogin(); return; }
  if (res.status === 428) { showChoose(await res.json()); return; }
  if (!res.ok) { showUnreachable(); return; }
  state.me = await res.json();
  state.devicesLoaded = false;
  socket.connect();
  // A computer's own window shows that computer (windows.js).
  if (ownComputer) setView('detail', ownComputer);
  else setView('list');
}

// Posts a sign-in. An answer that went through goes to `done(res)`; otherwise `error` says why:
// too many tries, `wrong` (the key of what was wrong), or no server.
async function postSignIn(body, error, wrong, done) {
  error.textContent = '';
  try {
    const res = await api('POST', '/api/login', body);
    if (res.ok) await done(res);
    else error.textContent = t(res.status === 429 ? 'err.tooMany' : wrong);
  } catch {
    error.textContent = t('err.noServer');
  }
}

// The sign-in page, over anything that was open (a session, a dialog). Returns its error line,
// for a reason to show at once.
export function showLogin() {
  currentSession()?.leave();
  closeDialogs();
  socket.close();
  setView('login');
  const user = h('input', { class: 'input', autocomplete: 'username', required: true });
  const pass = h('input', { class: 'input', type: 'password', autocomplete: 'current-password', required: true });
  const error = h('div', { class: 'error', role: 'alert' });
  const submit = h('button', { class: 'btn primary block', type: 'submit' }, t('login.submit'));
  const form = h('form', {
    class: 'form login',
    onsubmit: async (e) => {
      e.preventDefault();
      submit.disabled = true;
      await postSignIn({ user: user.value.trim(), password: pass.value }, error, 'login.wrong', async (res) => {
        if (res.status === 202) showWaiting((await res.json()).code, user.value.trim(), pass.value);
        else await start();
      });
      submit.disabled = false;
    },
  },
  // Opened from the Home Screen: signed in apart from Safari, and a new device.
  separateHomeScreenApp() ? h('div', { class: 'note-box', role: 'note' }, h('div', {}, h('b', {}, t('login.homeAppTitle')), t('login.homeAppText'))) : null,
  h('div', { class: 'fields' },
    h('label', {}, t('login.user'), user),
    h('label', {}, t('login.password'), pass)),
  error,
  submit);
  // A new language redraws the page, keeping the account name already typed.
  const redraw = () => {
    const typed = user.value;
    showLogin();
    const again = $app.querySelector('.login input');
    if (again) again.value = typed;
  };
  $app.replaceChildren(h('div', { class: 'page single' }, topbar(languagePicker(redraw)), heading(t('login.title')), form));
  user.focus();
  return error;
}

let waitTimer = null;

// A new device waits here. It first makes its passkey, then shows six digits made from that
// key; the person checks them on a device that is already signed in and approves there. The
// password stays in this closure only for the super-password path.
function showWaiting(serverCode, userName, password) {
  clearInterval(waitTimer);
  setView('waiting');
  const stop = () => clearInterval(waitTimer);
  waitTimer = setInterval(async () => {
    try {
      // Not through api(): this device is not signed in yet, so a 401 is no sign-out.
      const res = await fetch('/api/login/status');
      if (res.status === 204) {
        stop();
        await start();
      } else if (res.status === 410) {
        stop();
        showLogin().textContent = t('wait.rejected');
      }
    } catch { /* keep waiting through a network blip */ }
  }, 2000);

  const step = h('div', { class: 'wait-step' });
  const error = h('div', { class: 'error', role: 'alert' });
  const showCode = (code, note) => step.replaceChildren(
    h('p', { class: 'hint' }, t('wait.hint')),
    codeBox(code, t('wait.codeLabel', { code })),
    loading(t('wait.waiting')),
    note ? h('p', { class: 'hint' }, note) : null);
  const prepare = (name) => {
    step.replaceChildren(loading(t('wait.preparing')));
    ensurePasskey(name)
      .then(async (rec) => showCode(await keyCode(rec.key)))
      .catch(() => showCode(serverCode, t('wait.noKey')));
  };
  // A device with no key yet is named first, with the name its kind gives it filled in: the
  // one approving it, and its computers, see that name.
  if (storedPasskey()) prepare();
  else {
    const name = h('input', { class: 'input', value: deviceLabel(), maxlength: 40, required: true });
    step.replaceChildren(h('form', { class: 'name-step', onsubmit: (e) => { e.preventDefault(); prepare(name.value.trim() || deviceLabel()); } },
      h('div', { class: 'fields' }, h('label', {}, t('wait.name'), name)),
      h('button', { class: 'btn primary block', type: 'submit' }, t('common.continue'))));
    name.select();
  }

  const input = h('input', { class: 'input', type: 'password', autocomplete: 'off', required: true, placeholder: t('wait.superPlaceholder'), 'aria-label': t('wait.superPlaceholder') });
  const setup = h('form', {
    class: 'setup',
    hidden: true,
    onsubmit: async (e) => {
      e.preventDefault();
      await postSignIn({ user: userName, password, super_password: input.value }, error, 'wait.superWrong', async () => {
        stop();
        await start();
      });
    },
  }, h('p', { class: 'hint' }, t('wait.superHint')), input, h('button', { class: 'btn primary block', type: 'submit' }, t('wait.superSubmit')));
  const toggle = h('button', {
    class: 'btn text',
    type: 'button',
    onclick: () => { setup.hidden = false; toggle.hidden = true; input.focus(); },
  }, t('wait.noOther'));
  $app.replaceChildren(h('div', { class: 'page single' }, topbar(), heading(t('wait.title')), h('div', { class: 'form' },
    step,
    error,
    toggle,
    setup,
    h('button', { class: 'btn text', type: 'button', onclick: () => { stop(); showLogin(); } }, t('wait.back')))));
}

// The account signed in with an initial password: it chooses its own (twice), and the first
// time its super password too, before any other page opens. `needs.super`: the super password
// is to be chosen as well.
export function showChoose(needs) {
  currentSession()?.leave();
  closeDialogs();
  socket.close();
  setView('choose');
  const secret = (key) => h('input', { class: 'input', type: 'password', autocomplete: 'new-password', required: true, 'aria-label': t(key) });
  const label = (key, input) => h('label', {}, t(key), input);
  const pass = secret('choose.password');
  const again = secret('choose.again');
  const superPass = needs.super ? secret('choose.super') : null;
  const superAgain = needs.super ? secret('choose.superAgain') : null;
  const error = h('div', { class: 'error', role: 'alert' });
  const submit = h('button', { class: 'btn primary block', type: 'submit' }, t('choose.submit'));
  const say = (key) => { error.textContent = t(key); };
  const form = h('form', {
    class: 'form choose',
    onsubmit: async (e) => {
      e.preventDefault();
      error.textContent = '';
      if (pass.value.length < 8) return say('pw.tooShort');
      if (pass.value !== again.value) return say('pw.mismatch');
      if (superPass && superPass.value.length < 8) return say('super.tooShort');
      if (superPass && superPass.value !== superAgain.value) return say('super.mismatch');
      submit.disabled = true;
      try {
        const res = await api('POST', '/api/initial-password', { new: pass.value, super_password: superPass?.value });
        if (res.ok) { await start(); return; }
        say(res.status === 409 ? 'choose.same' : 'err.notSaved');
      } catch (err) {
        if (!signedOut(err)) say('err.noServer');
      }
      submit.disabled = false;
    },
  },
  h('p', { class: 'hint' }, t('choose.intro')),
  h('div', { class: 'fields' }, label('choose.password', pass), label('choose.again', again)),
  superPass ? h('p', { class: 'hint choose-super' }, t('choose.superIntro')) : null,
  superPass ? h('div', { class: 'fields' }, label('choose.super', superPass), label('choose.superAgain', superAgain)) : null,
  error,
  submit,
  h('button', { class: 'btn text', type: 'button', onclick: async () => { await api('POST', '/api/logout').catch(() => {}); showLogin(); } }, t('menu.signOut')));
  $app.replaceChildren(h('div', { class: 'page single', 'data-page': 'choose' }, topbar(), heading(t('choose.title')), form));
  pass.focus();
}

export function showUnreachable() {
  $app.replaceChildren(h('div', { class: 'page single' }, topbar(), heading(t('unreach.title')),
    h('p', { class: 'lead' }, t('unreach.text')),
    h('div', { class: 'actions-row' }, h('button', { class: 'btn primary', onclick: () => location.reload() }, t('common.retry')))));
  // The texts too, should they not have arrived the first time.
  setTimeout(() => { if (!state.me) setLanguage(currentLang()).then(start).catch(showUnreachable); }, 5000);
}
