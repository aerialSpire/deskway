// Browsers and computers waiting to be let into the account, asked one at a time. An AI
// client's request is answered on the Add an AI client page while it is open (ai-client.js), with
// the same answers (answerDevice).
//
// Public surface: showNextApproval(), deviceOf(a), answerDevice(a, dev, allow, alsoOff).
import { $dialogs, h } from '../dom.js';
import { t } from '../i18n.js';
import { api, signedOut } from '../net.js';
import { signWith, setPolicy } from '../auth.js';
import { keyCode, endorseChallenge } from '../auth-messages.js';
import { state } from '../state.js';
import { dialog, gotIt, closeDialogs, dialogOpen } from '../dialogs.js';
import { codeBox } from './common.js';

// The waiting device's key, once it has given one; null before.
export function deviceOf(a) {
  if (a.kind !== 'browser' || !a.device) return null;
  try { return typeof a.device === 'string' ? JSON.parse(a.device) : a.device; } catch { return null; }
}

// Shown on its own page instead of in a dialog.
const onItsPage = (a) => state.view === 'aiclient' && deviceOf(a)?.role === 'ai';

export function showNextApproval() {
  // Full screen hides the dialog layer, so a request waits until the viewer leaves it.
  if (!state.me || document.fullscreenElement) return;
  const open = $dialogs.querySelector('[data-approval]');
  if (open) {
    const same = state.approvals.find((a) => a.id === open.dataset.approval);
    // Still waiting for the new device's passkey, which has now arrived: show its code.
    if (same && !onItsPage(same) && !(open.dataset.nokey && same.device)) return;
    closeDialogs(); // answered on another device, ready to show its code, or now on its page
  }
  if (dialogOpen()) return;
  const a = state.approvals.find((x) => !onItsPage(x));
  if (!a) return;
  const deny = () => answerDevice(a, null, false);
  if (a.kind === 'browser') {
    showDeviceRequest(a, deny);
  } else {
    const actions = [{ label: t('common.deny'), run: deny }, { label: t('common.approve'), primary: true, run: () => answerDevice(a, null, true) }];
    dialog(t('approve.computerTitle'), t('approve.computerText', { label: a.label, ip: a.ip }), actions).dataset.approval = a.id;
  }
}

// Answers a waiting browser, AI client or computer. A device with a key is approved with this
// device's signature over it (and, if asked, device checks turned off after); then the person is
// told it was added.
export async function answerDevice(a, dev, allow, alsoOff = false) {
  state.approvals = state.approvals.filter((x) => x.id !== a.id);
  if (!allow || !dev) {
    api('POST', '/api/approvals', { kind: a.kind, id: a.id, allow }).catch(() => {});
    return;
  }
  try {
    const proof = await signWith((ts) => endorseChallenge(dev, ts));
    const res = await api('POST', '/api/approvals', { kind: 'browser', id: a.id, allow: true, endorse: proof });
    if (!res.ok) throw new Error('approve');
    if (alsoOff) await setPolicy(false);
    dialog(t('approve.addedTitle'), t(dev.role === 'ai' ? 'approve.aiAddedText' : 'approve.addedText', { name: dev.name }), [{ label: t('common.ok') }]);
  } catch (e) {
    if (!signedOut(e)) gotIt(t('approve.failedTitle'), t('approve.failedText'));
  }
}

// A new phone or computer asking to join, or an AI client's deskway. The digits come from its key
// (and an AI client's role), computed here, so they match only if this is really the device the
// person is holding. An AI client is said to be one: it reaches only the computers the person
// allows AI on, and turning device checks off is not offered with it.
async function showDeviceRequest(a, deny) {
  const dev = deviceOf(a);
  if (!dev) {
    const d = dialog(t('approve.signingInTitle'), t('approve.signingInText', { label: a.label, ip: a.ip }), [
      { label: t('common.deny'), run: deny },
    ]);
    d.dataset.approval = a.id;
    d.dataset.nokey = '1';
    return;
  }
  const ai = dev.role === 'ai';
  const code = await keyCode(dev.key, dev.role);
  if (!state.approvals.some((x) => x.id === a.id) || dialogOpen()) return;
  const off = h('input', { type: 'checkbox' });
  dialog(t(ai ? 'approve.aiTitle' : 'approve.deviceTitle', { name: dev.name }), h('div', {},
    h('p', {}, t('approve.deviceFrom', { ip: a.ip })),
    codeBox(code),
    h('p', {}, t(ai ? 'approve.aiWarn' : 'approve.deviceWarn')),
    ai ? null : h('label', { class: 'check' }, off, t('approve.alsoOff'))), [
    { label: t('common.deny'), run: deny },
    { label: t('common.approve'), primary: true, run: () => answerDevice(a, dev, true, off.checked) },
  ]).dataset.approval = a.id;
}
