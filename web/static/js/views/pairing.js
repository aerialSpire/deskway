// Adding this device to a computer: with the computer's QR code, or by asking the person
// sitting at it. `open(d)` is what follows when the device may control the computer.
import { h, loading } from '../dom.js';
import { randomId, sessionStore } from '../env.js';
import { t } from '../i18n.js';
import { socket, signedOut } from '../net.js';
import { ensurePasskey, deviceLabel, vouchFor, pairProof, pinHostKey, setPolicy } from '../auth.js';
import { keyCode, normalizeCode } from '../auth-messages.js';
import { Signal } from '../proto.js';
import { state, setView } from '../state.js';
import { dialog, gotIt, closeDialogs } from '../dialogs.js';
import { codeBox, noDesktop, showNoDesktop } from './common.js';

// Pairing requests waiting for the computer's reply, by session id.
const pairings = new Map();

// A reply from a computer (`paired`, or `bye` with an error). True when it was for a pairing.
export function onPairReply(m) {
  const handler = pairings.get(m.session);
  if (!handler) return false;
  handler(m);
  return true;
}

// Pairing failures by code (from this page or the computer), each with its message `pair.<code>`.
const PAIR_ERRORS = new Set(['format', 'pair', 'denied', 'full', 'mac', 'timeout', 'offline', 'forbidden', 'nokey']);
// The message for a failure, or the general one when its code is unknown.
const pairError = (...codes) => {
  const code = codes.find((c) => PAIR_ERRORS.has(c));
  return code ? t(`pair.${code}`) : t('common.tryAgain');
};

async function pairDevice(d, code) {
  const ncode = normalizeCode(code);
  if (ncode.length !== 16) throw new Error('format');
  const rec = await ensurePasskey();
  const name = rec.name || deviceLabel();
  const vouch = await vouchFor(d.id, rec);
  const proof = await pairProof(d.id, rec, name, ncode);
  const session = randomId();
  const reply = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => { pairings.delete(session); reject(new Error('timeout')); }, 20000);
    pairings.set(session, (m) => { clearTimeout(timer); pairings.delete(session); resolve(m); });
    socket.send({ type: Signal.PAIR, session, device: d.id, auth: { cred: rec.cred, alg: rec.alg, key: rec.key, name, mac: proof.mac, vouch } });
  });
  if (reply.type !== Signal.PAIRED) throw new Error(reply.reason || 'pair');
  const hk = reply.auth && reply.auth.hk;
  if (!(await proof.replyValid(hk, reply.auth && reply.auth.mac))) throw new Error('mac');
  // The QR code vouches for this key, so it replaces whatever was pinned before.
  pinHostKey(d.id, hk);
}

// Asks the person sitting at the computer: its panel shows this device and six digits.
async function requestControl(d, open) {
  if (noDesktop(state.devices.find((x) => x.id === d.id) || d)) { showNoDesktop(); return; }
  let rec;
  let vouch;
  try {
    rec = await ensurePasskey();
    vouch = await vouchFor(d.id, rec);
  } catch (e) {
    gotIt(t('ask.notSentTitle'), pairError(e.name, e.message));
    return;
  }
  const code = await keyCode(rec.key);
  const session = randomId();
  let finished = false;
  const stop = () => { finished = true; pairings.delete(session); clearTimeout(timer); };
  const timer = setTimeout(() => { if (!finished) { stop(); gotIt(t('ask.noAnswerTitle'), t('ask.noAnswerText')); } }, 5 * 60 * 1000);
  pairings.set(session, (m) => {
    stop();
    if (m.type === Signal.PAIRED && m.auth && m.auth.hk) {
      pinHostKey(d.id, m.auth.hk);
      closeDialogs();
      open(d);
    } else {
      gotIt(t('approve.failedTitle'), pairError(m.reason));
    }
  });
  socket.send({ type: Signal.PAIR_ASK, session, device: d.id, auth: { cred: rec.cred, alg: rec.alg, key: rec.key, name: rec.name || deviceLabel(), vouch } });
  dialog(t('ask.waitTitle', { name: d.name }), h('div', {},
    h('p', {}, t('ask.waitText')),
    codeBox(code),
    loading(t('ask.waiting'))), [
    { label: t('common.cancel'), run: stop },
  ]);
}

// This device cannot control the computer yet: say how to change that, in one screen.
export function showNotAdded(d, open) {
  if (noDesktop(d)) { showNoDesktop(); return; }
  dialog(t('notAdded.title', { name: d.name }), h('div', {},
    h('p', {}, t('notAdded.intro')),
    h('p', {}, t('notAdded.qr')),
    h('p', {}, t('notAdded.ask')),
    h('p', { class: 'hint' }, t('notAdded.hint'))), [
    { label: t('common.cancel') },
    { label: t('notAdded.request'), primary: true, run: () => requestControl(d, open) },
  ]);
}

// A computer's QR code opens #pair=<computer>.<code>. The code never reaches the server (it is
// after the #) and is removed from the address bar at once; it waits here until signed in.
export function takePairLink() {
  const m = /^#pair=([^.]+)\.([0-9A-Z]+)$/i.exec(location.hash);
  if (!m) return;
  sessionStore.set('deskway.pair', `${m[1]}.${m[2]}`);
  history.replaceState(null, '', location.pathname);
}

// Opened from a computer's QR code: offers to add this device to it, once signed in.
export function offerQrPair(open) {
  const pending = sessionStore.get('deskway.pair');
  if (!pending || !state.devicesLoaded) return;
  sessionStore.remove('deskway.pair');
  const [hostId, code] = pending.split('.');
  const d = state.devices.find((x) => x.id === hostId);
  if (!d) {
    gotIt(t('qr.notFoundTitle'), t('qr.notFoundText'));
    return;
  }
  if (noDesktop(d)) { showNoDesktop(); return; }
  const off = h('input', { type: 'checkbox' });
  dialog(t('qr.title', { name: d.name }), h('div', {},
    h('p', {}, t('qr.text')),
    h('label', { class: 'check' }, off, t('qr.alsoOff'))), [
    { label: t('common.cancel') },
    { label: t('qr.add'), primary: true, run: async () => {
      if (noDesktop(state.devices.find((x) => x.id === d.id) || d)) { showNoDesktop(); return; }
      try {
        await pairDevice(d, code);
        if (off.checked) await setPolicy(false);
        dialog(t('approve.addedTitle'), t('qr.addedText', { name: d.name }), [{ label: t('detail.takeOver'), primary: true, run: () => { setView('detail', d.id); open(d); } }, { label: t('common.ok') }]);
      } catch (e) {
        if (signedOut(e)) return;
        // A failure without a known code (a raw error) gets the general message.
        gotIt(t('approve.failedTitle'), PAIR_ERRORS.has(e.name) || PAIR_ERRORS.has(e.message) ? pairError(e.name, e.message) : t('qr.failed'));
      }
    } },
  ]);
}
