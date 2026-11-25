// Pressing a computer's picture and what a session's end says (docs/session-lifecycle.md):
// the one question asked when someone holds the computer (from the device list's `controller`),
// and the one mapping from every reason a session ends or does not start (the wire's, and this
// page's own: `unreachable`, `nokey`, `keyChanged`, `forged`, `old`) to what is shown and what
// can be done.
//
// A computer's own window (windows.js) takes it over as it opens, and closes when the person
// ends the session there.
//
// Public surface: press(d, again), openSession(d, force), startOwnWindow().
import { t, timeText } from '../i18n.js';
import { socket } from '../net.js';
import { state, setView, currentSession, setCurrentSession, device } from '../state.js';
import { Session } from '../session/session.js';
import { dialog, gotIt } from '../dialogs.js';
import { showNotAdded } from './pairing.js';
import { MIN_VERSION } from '../proto.js';
import { ownComputer, openWindow } from '../windows.js';

// Pressing a computer's picture (and every Retry / reconnect after an end, `again`: chosen
// already). Must run inside the tap: the session goes full screen at once.
export function press(d, again = false) {
  if (!d.online) { show(d, { reason: 'offline' }); return; }
  // The page knows its socket is down: nothing can start now, and it says so at once.
  if (!socket.isOpen()) { show(d, { reason: 'unreachable' }); return; }
  if (openWindow(d)) return;
  // The computer this browser runs on (here.js): asked once, then as any other.
  if (d.here && !again) {
    dialog(t('here.confirmTitle'), t('here.confirmText'), [
      { label: t('common.cancel') },
      { label: t('here.confirmGo'), primary: true, run: () => press(d, true) },
    ]);
    return;
  }
  if (d.controller) ask(d, d.controller);
  else openSession(d, false);
}

// Someone holds the computer: exactly one question, by who. A session of this device that is
// still connecting is replaced by the computer without asking.
function ask(d, c) {
  if (c.device && c.connecting) { openSession(d, false); return; }
  let q;
  if (c.device) q = { title: t('force.deviceTitle'), text: t('force.deviceText'), go: t('force.deviceGo') };
  else q = { title: t(c.connecting ? 'force.accountConnecting' : 'force.account', { name: c.name }), text: t('force.accountText'), go: t('force.accountGo') };
  dialog(q.title, q.text, [
    { label: t('common.cancel') },
    { label: q.go, primary: true, run: () => openSession(d, true) },
  ]);
}

export function openSession(d, force) {
  // A delayed action may still run after a new connection has started on this page.
  if (currentSession()) return;
  // A delayed action uses the latest report from the online computer's current socket.
  const live = state.devices.find((x) => x.id === d.id);
  if (live?.online && live.version && live.version < MIN_VERSION) { show(d, { reason: 'old' }); return; }
  setView('session', d.id);
  // Full screen needs a tap; a computer's own window takes it over as it opens, without one:
  // there “Full screen” is pressed by hand.
  const full = !ownComputer || (navigator.userActivation?.isActive ?? true);
  const s = new Session(d, { force, full, onEnd: (end) => ended(s, end) });
  setCurrentSession(s);
  // The phone's back gesture (or the browser's back button) ends this session, like “Disconnect”.
  history.pushState({ deskway: 'session', id: s.id }, '');
  shown = history.state;
}

// The history entry showing now. Going back ends the session whose id the entry left carried,
// and only that one: a session opened meanwhile is not touched.
let shown = history.state;
window.addEventListener('popstate', () => {
  const left = shown;
  shown = history.state;
  const s = currentSession();
  if (left && left.deskway === 'session' && s && s.id === left.id) s.leave();
});

// Back on the computer's page, saying why when it was not the person's own choice.
function ended(s, end) {
  setCurrentSession(null);
  // Ended by the person in the computer's own window: the window goes (only a window opened by
  // the page can be closed; another stays, on the computer's page).
  if (end.reason === 'left' && s.device.id === ownComputer) window.close();
  setView('detail', s.device.id);
  // Its history entry goes too, unless going back is what ended it.
  if (history.state && history.state.deskway === 'session' && history.state.id === s.id) history.back();
  show(device() || s.device, end);
}

// A computer's own window, once the computers are known: its name in the title, and taken over.
let ownStarted = false;
export function startOwnWindow() {
  if (!ownComputer || ownStarted) return;
  const d = state.devices.find((x) => x.id === ownComputer);
  if (!d) return;
  ownStarted = true;
  document.title = `${d.name} · Deskway`;
  press(d);
}

// The one mapping from a reason to what the page shows and what can be done next.
function show(d, { reason, by, same, controller, via, established, cause }) {
  const again = (label) => ({ label, primary: true, run: () => press(device() || d, true) });
  const back = { label: t('common.back') };
  switch (reason) {
    case 'left':
      break;
    // Before anything starts.
    case 'unreachable': gotIt(t('session.unreachableTitle'), t('session.unreachableText')); break;
    case 'old': gotIt(t('update.computerTitle'), t('update.computerText')); break;
    case 'nokey': gotIt(t('session.cantConnect'), t('pair.nokey')); break;
    // Someone holds it after all: the question for who (the server describes the holder).
    case 'busy': ask(d, controller); break;
    // C1: it could not be established or recovered, or is not known any more.
    case 'failed':
    case 'gone':
      if (established && cause === 'host') dialog(t('session.offlineTitle'), t('session.offlineText'), [back, again(t('session.reconnect'))]);
      else if (established) dialog(t('session.failedTitle'), t('session.failedText'), [back, again(t('session.reconnect'))]);
      else dialog(t('session.notConnected'), t('session.notConnectedText'), [back, again(t('common.retry'))]);
      break;
    case 'offline': dialog(t('session.offlineTitle'), t('session.offlineText'), [back, again(t('session.reconnect'))]); break;
    // C2: taken over; never reconnecting by itself.
    case 'taken':
      dialog(same ? t('session.takenHereTitle') : t('session.takenTitle', { name: by || t('session.someone') }), t('session.takenText', { time: timeText(new Date()) }), [
        { label: t('session.backToList'), run: () => setView('list') },
        { label: t('session.takeBack'), primary: true, run: () => openSession(device() || d, true) },
      ]);
      break;
    // C3: the computer ended it. Told by the server, while this page was away from it: the
    // computer restarted (its status no longer names the session).
    case 'ended':
      if (via === 'bye' && established) dialog(t('session.restartedTitle'), t('session.restartedText'), [back, again(t('session.reconnect'))]);
      else gotIt(t('session.endedTitle'), t('session.endedText'));
      break;
    case 'permissions': gotIt(t('session.permissionsTitle'), t('session.permissionsText')); break;
    case 'forbidden': gotIt(t('session.forbiddenTitle'), t('session.forbiddenText')); break;
    // C4: add this device again (or sign in again, which the sign-in page does by itself).
    case 'unpaired': showNotAdded(d, (x) => openSession(x, false)); break;
    case 'auth':
      gotIt(t('session.authTitle'), t('session.authText'),
        { also: { label: t('session.addAgain'), run: () => showNotAdded(d, (x) => openSession(x, false)) } });
      break;
    // The computer's identity is not confirmed: someone may be posing as it. Adding it again is
    // offered only for a changed key (Deskway reinstalled there), never for a forged answer.
    case 'keyChanged':
      gotIt(t('session.impostorTitle'), t('session.keyChanged'),
        { also: { label: t('session.addAgain'), run: () => showNotAdded(d, (x) => openSession(x, false)) } });
      break;
    case 'forged': gotIt(t('session.impostorTitle'), t('session.forged')); break;
    default: gotIt(t('session.cantConnect'), t('common.tryAgain'));
  }
}
