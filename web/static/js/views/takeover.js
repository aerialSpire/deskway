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
import { state, setView, currentSession, setCurrentSession, endCurrentSession, device } from '../state.js';
import { Session } from '../session/session.js';
import { dialog, dialogOpen, gotIt } from '../dialogs.js';
import { showNotAdded } from './pairing.js';
import { noDesktop, showNoDesktop } from './common.js';
import { offerMessage, takeoverMessage, workspaceNotice } from './offer-message.js';
import { CAPS, MIN_VERSION } from '../proto.js';
import { ownComputer, openWindow, onWindowView } from '../windows.js';

// Pressing a computer's picture (and every Retry / reconnect after an end, `again`: chosen
// already). Must run inside the tap: the session goes full screen at once.
export function press(d, again = false, view = 'desktop', target) {
  if (!d.online) { show(d, { reason: 'offline' }, view, target); return; }
  if (!offered(d, view)) return;
  // The page knows its socket is down: nothing can start now, and it says so at once.
  if (!socket.isOpen()) { show(d, { reason: 'unreachable' }, view, target); return; }
  if (openWindow(d, view, target)) return;
  // The computer this browser runs on (here.js): asked once, then as any other.
  if (d.here && !again) {
    dialog(t('here.confirmTitle'), t('here.confirmText'), [
      { label: t('common.cancel') },
      { label: t('here.confirmGo'), primary: true, run: () => press(d, true, view, target) },
    ]);
    return;
  }
  // A report naming this device may describe another tab or a session that just ended.
  if (view === 'desktop' && d.controller && !d.controller.device) showOfferMessage(d, view, takeoverMessage(d.controller, t));
  else openSession(d, false, view, target);
}

function offered(d, view) {
  if (view !== 'desktop') {
    if (workspaceNotice(d.workspace_access, t)) {
      show(d, { reason: d.workspace_access }, view);
      return false;
    }
    if (d.caps?.includes(view === 'terminal' ? CAPS.TERMINAL : CAPS.BROWSE)) return true;
    if (view === 'files') gotIt(t('files.unavailable'));
    else show(d, { reason: 'no_terminal' }, view);
    return false;
  }
  if (noDesktop(d)) { showNoDesktop(); return false; }
  return true;
}

export function openSession(d, force, view = 'desktop', target) {
  // A delayed action may still run after a new connection has started on this page.
  if (currentSession()) return;
  // A delayed action uses the latest report from the online computer's current socket.
  const deviceStatus = () => state.devices.find((x) => x.id === d.id);
  const live = deviceStatus();
  if (live?.online && live.version && live.version < MIN_VERSION) { show(d, { reason: 'old' }, view); return; }
  if (!offered(live || d, view)) return;
  setView('session', d.id);
  // Full screen needs a tap; a computer's own window takes it over as it opens, without one:
  // there “Full screen” is pressed by hand.
  const full = !ownComputer || (navigator.userActivation?.isActive ?? true);
  const s = new Session(live || d, { force, view, target, deviceStatus, full, onEnd: (end) => ended(s, end) });
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
  if (!endCurrentSession(s)) return;
  // Ended by the person in the computer's own window: the window goes (only a window opened by
  // the page can be closed; another stays, on the computer's page).
  if (end.reason === 'left' && s.device.id === ownComputer) window.close();
  setView('detail', s.device.id);
  // Its history entry goes too, unless going back is what ended it.
  if (history.state && history.state.deskway === 'session' && history.state.id === s.id) history.back();
  show(device() || s.device, end, s.content.currentView || 'desktop', s.content.terminal?.selection);
}

onWindowView((view, target) => {
  if (view !== 'terminal' && view !== 'files') return;
  const session = currentSession();
  if (session) {
    if (session.content.currentView && (view === 'terminal' || session.hasCap(CAPS.BROWSE))) session.content.show(view, target);
  } else if (!dialogOpen()) {
    const d = state.devices.find((d) => d.id === ownComputer);
    if (d) press(d, false, view, target);
  }
});

// A computer's own window, once the computers are known: its name in the title, and taken over.
let ownStarted = false;
export function startOwnWindow() {
  if (!ownComputer || ownStarted) return;
  const d = state.devices.find((x) => x.id === ownComputer);
  if (!d) return;
  ownStarted = true;
  document.title = `${d.name} · Deskway`;
  const params = new URLSearchParams(location.search);
  const view = params.get('view');
  const terminal = params.get('terminal');
  const id = Number(terminal);
  const target = terminal === 'new' ? null : id > 0 && Number.isSafeInteger(id) ? id : undefined;
  press(d, false, ['terminal', 'files'].includes(view) ? view : 'desktop', target);
}

function showOfferMessage(d, view, { action, title, text, go }, target) {
  const pair = () => showNotAdded(d, (x) => openSession(x, false, view, target));
  switch (action) {
    case 'reconnect':
    case 'retry':
      dialog(title, text, [
        { label: t('common.back') },
        { label: t(action === 'reconnect' ? 'session.reconnect' : 'common.retry'), primary: true, run: () => press(device() || d, true, view, target) },
      ]);
      break;
    case 'desktop':
      dialog(title, text, [
        { label: t('common.back') },
        { label: t('detail.hint'), primary: true, run: () => press(device() || d, true) },
      ]);
      break;
    case 'replace': openSession(d, false, view, target); break;
    case 'takeover':
      dialog(title, text, [
        { label: t('common.cancel') },
        { label: go, primary: true, run: () => openSession(d, true, view, target) },
      ]);
      break;
    case 'takeBack':
      dialog(title, text, [
        { label: t('session.backToList'), run: () => setView('list') },
        { label: t('session.takeBack'), primary: true, run: () => openSession(device() || d, true, view, target) },
      ]);
      break;
    case 'pair': pair(); break;
    case 'addAgain': gotIt(title, text, { also: { label: t('session.addAgain'), run: pair } }); break;
    case 'devices': gotIt(title, text, { also: { label: t('devices.title'), primary: true, run: () => setView('devices') } }); break;
    default: gotIt(title, text);
  }
}

function show(d, end, view, target) {
  const { reason } = end;
  const message = offerMessage(end, { view, headless: noDesktop(d), t, time: () => timeText(new Date()) });
  if (message) { showOfferMessage(d, view, message, target); return; }
  switch (reason) {
    case 'left':
      break;
    // Before anything starts.
    case 'unreachable': gotIt(t('session.unreachableTitle'), t('session.unreachableText')); break;
    case 'old': gotIt(t('update.computerTitle'), t('update.computerText')); break;
    case 'nokey': gotIt(t('session.cantConnect'), t('pair.nokey')); break;
    case 'no_desktop': showNoDesktop(); break;
    case 'forbidden': gotIt(t('session.forbiddenTitle'), t('session.forbiddenText')); break;
    // The computer's identity is not confirmed: someone may be posing as it. Adding it again is
    // offered only for a changed key (Deskway reinstalled there), never for a forged answer.
    case 'keyChanged':
      gotIt(t('session.impostorTitle'), t('session.keyChanged'),
        { also: { label: t('session.addAgain'), run: () => showNotAdded(d, (x) => openSession(x, false, view, target)) } });
      break;
    case 'forged': gotIt(t('session.impostorTitle'), t('session.forged')); break;
    default: gotIt(t('session.cantConnect'), t('common.tryAgain'));
  }
}
