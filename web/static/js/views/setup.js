import { $app, h, loading } from '../dom.js';
import { deviceLabel, ensurePasskey, storedPasskey } from '../auth.js';
import { hexOf, sha256 } from '../auth-messages.js';
import { t } from '../i18n.js';
import { api } from '../net.js';
import { setView } from '../state.js';
import { heading, topbar } from './common.js';

async function prepareDevice() {
  const key = await ensurePasskey();
  if (storedPasskey()?.cred === key.cred) return { ...key, name: key.name || deviceLabel() };
}

export function captureSetupLink(prepare = prepareDevice) {
  if (location.pathname !== '/setup') return null;
  let code = new URLSearchParams(location.hash.slice(1)).get('c');
  history.replaceState(history.state, '', location.pathname + location.search);
  let submitting = false;
  return {
    async details() {
      if (!code) return new Response(null, { status: 410 });
      const digest = hexOf(await sha256(code));
      return api('GET', '/api/setup?c=' + digest);
    },
    async redeem(fields) {
      if (!code || submitting) return;
      submitting = true;
      try {
        const device = await prepare().catch(() => null);
        if (!device) return new Response(null, { status: 422 });
        const res = await api('POST', '/api/setup', { code, ...fields, device });
        if (res.ok || res.status === 410) code = null;
        return res;
      } finally {
        submitting = false;
      }
    },
  };
}

export async function showSetup(link, signedIn) {
  setView('setup');
  const page = (title, body) =>
    $app.replaceChildren(h('div', { class: 'page single', 'data-page': 'setup' }, topbar(), heading(t(title)), body));
  const back = () => h('a', { class: 'btn text', href: '/' }, t('wait.back'));
  const statusKey = (status) => {
    if (status === 410) return 'setup.invalid';
    if (status === 422) return 'pair.nokey';
    if (status === 429) return 'err.tooMany';
    return 'err.notSaved';
  };
  const failure = (key) => page('setup.title', h('div', { class: 'form' }, h('div', { class: 'error', role: 'alert' }, t(key)), back()));
  page('setup.title', loading(t('common.loading')));
  let details;
  try {
    const res = await link.details();
    if (!res.ok) return failure(statusKey(res.status));
    details = await res.json();
  } catch {
    return failure('err.noServer');
  }

  const redeem = async (fields) => {
    const res = await link.redeem(fields);
    if (res.ok) {
      history.replaceState(history.state, '', '/');
      await signedIn();
    }
    return res;
  };
  if (details.kind === 'login') {
    page('setup.loginTitle', loading(t('common.loading')));
    try {
      const res = await redeem({});
      if (!res.ok) failure(statusKey(res.status));
    } catch {
      failure('err.noServer');
    }
    return;
  }

  const name = h('input', { class: 'input', autocomplete: 'username', maxlength: 32, required: true, autocapitalize: 'none', spellcheck: 'false' });
  const display = h('input', { class: 'input', autocomplete: 'nickname', maxlength: 20 });
  const secret = () => h('input', { class: 'input', type: 'password', autocomplete: 'new-password', required: true });
  const pass = secret();
  const again = secret();
  const error = h('div', { class: 'error', role: 'alert' });
  const submit = h('button', { class: 'btn primary block', type: 'submit' }, t('setup.submit'));
  const say = (key) => { error.textContent = t(key); };
  const form = h('form', {
    class: 'form setup',
    onsubmit: async (e) => {
      e.preventDefault();
      if (submit.disabled) return;
      error.textContent = '';
      if (!details.name && !/^[A-Za-z0-9._-]{2,32}$/.test(name.value.trim())) return say('signin.rule');
      if (pass.value.length < 8) return say('pw.tooShort');
      if (new TextEncoder().encode(pass.value).length > 72) return say('pw.tooLong');
      if (pass.value !== again.value) return say('pw.mismatch');
      submit.disabled = true;
      try {
        const res = await redeem({
          password: pass.value,
          name: details.name ? undefined : name.value.trim(),
          display: display.value.trim(),
        });
        if (res.ok) return;
        if (res.status === 410) return failure('setup.invalid');
        say(res.status === 409 ? 'signin.taken' : statusKey(res.status));
      } catch {
        say('err.noServer');
      }
      submit.disabled = false;
    },
  },
  h('div', { class: 'fields' },
    details.name ? h('label', {}, t('login.user'), h('input', { class: 'input', autocomplete: 'username', value: details.name, readonly: true }))
      : h('label', {}, t('login.user'), name, h('span', { class: 'hint' }, t('signin.rule'))),
    h('label', {}, t('members.display'), display),
    h('label', {}, t('pw.new'), pass),
    h('label', {}, t('pw.again'), again)),
  error, submit, back());
  page(details.kind === 'reset' ? 'setup.resetTitle' : 'setup.title', form);
  (details.name ? pass : name).focus();
}
