// Signing in with a password and, for a new browser, approval from a signed-in device.
import { $app, h, loading } from '../dom.js';
import { separateHomeScreenApp } from '../env.js';
import { t, currentLang, setLanguage } from '../i18n.js';
import { api, socket } from '../net.js';
import { ensurePasskey, reportPasskey, storedPasskey, deviceLabel } from '../auth.js';
import { keyCode } from '../auth-messages.js';
import { browserSignIn } from '../browser-sign-in.js';
import { state, setView, currentSession } from '../state.js';
import { closeDialogs } from '../dialogs.js';
import { ownComputer } from '../windows.js';
import { languagePicker, topbar, heading, codeBox } from './common.js';
import { mcpRequest } from '../mcp-approval.js';
import { mcpSignInNotice } from './mcp-approval.js';

// Not through api(): a 401 here is the answer asked for.
const getSession = () => fetch('/api/me');
const signIn = browserSignIn({ getSession, ensurePasskey, reportPasskey, keyCode });

// Finds out who is signed in, then shows the computers, or the sign-in page.
export async function start(afterSetup = false) {
  let res;
  try {
    res = afterSetup ? await getSession() : await signIn.current();
  } catch {
    // Not signed out: the server is unreachable. Saying "sign in" here would be wrong.
    showUnreachable();
    return;
  }
  if (res.status === 401) { showLogin(); return; }
  if (!res.ok) { showUnreachable(); return; }
  state.me = await res.json();
  if (mcpRequest(location)) { setView('mcp-approval'); return; }
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
        if (res.status === 202) showWaiting((await res.json()).code);
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
  submit, mcpSignInNotice());
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
// key; the person checks them on a device that is already signed in and approves there.
function showWaiting(serverCode) {
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
  const showCode = (code, note) => step.replaceChildren(
    h('p', { class: 'hint' }, t('wait.hint')),
    codeBox(code, t('wait.codeLabel', { code })),
    loading(t('wait.waiting')),
    note ? h('p', { class: 'hint' }, note) : null);
  const prepare = (name) => {
    step.replaceChildren(loading(t('wait.preparing')));
    signIn.waiting(name, serverCode).then(({ code, noKey }) => showCode(code, noKey ? t('wait.noKey') : undefined));
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

  $app.replaceChildren(h('div', { class: 'page single' }, topbar(), heading(t('wait.title')), h('div', { class: 'form' },
    mcpSignInNotice(),
    step,
    h('button', { class: 'btn text', type: 'button', onclick: () => { stop(); showLogin(); } }, t('wait.back')))));
}

export function showUnreachable() {
  $app.replaceChildren(h('div', { class: 'page single' }, topbar(), heading(t('unreach.title')),
    h('p', { class: 'lead' }, t('unreach.text')),
    h('div', { class: 'actions-row' }, h('button', { class: 'btn primary', onclick: () => location.reload() }, t('common.retry')))));
  // The texts too, should they not have arrived the first time.
  setTimeout(() => { if (!state.me) setLanguage(currentLang()).then(start).catch(showUnreachable); }, 5000);
}
