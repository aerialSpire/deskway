// Each computer in a window of its own, when this device chose so (My devices: Each computer in its
// own window; off unless turned on). A computer's window is this page opened with ?computer=<id>: it
// shows that computer and takes it over as it opens (takeover.js). Pressing a computer
// elsewhere brings its window to the front when it is open, else opens it. The windows of this
// browser find each other over a BroadcastChannel: each computer's window says it is there, and
// answers when asked to come to the front.
//
// Public surface: ownComputer (the computer this window is for, or null), openWindow(d) (true:
// handled here).
import { store } from './env.js';
import { gotIt } from './dialogs.js';
import { t } from './i18n.js';

export const ownComputer = new URLSearchParams(location.search).get('computer');

const channel = 'BroadcastChannel' in window ? new BroadcastChannel('deskway.windows') : null;
// Windows this page opened, by computer: brought to the front directly.
const opened = new Map();
// Computers whose window has said it is open.
const open = new Set();
// Answers to a request to come to the front, by computer.
const waiting = new Map();
// How long a window has to answer before it is taken to be gone (it closed without saying).
const ANSWER_MS = 300;
let selectView = () => {};

export function onWindowView(fn) { selectView = fn; }

if (channel) {
  channel.onmessage = ({ data: m }) => {
    if (m.t === 'who' && ownComputer) channel.postMessage({ t: 'open', id: ownComputer });
    else if (m.t === 'open') open.add(m.id);
    else if (m.t === 'closed') open.delete(m.id);
    else if (m.t === 'front' && m.id === ownComputer) {
      channel.postMessage({ t: 'here', id: ownComputer });
      window.focus();
      selectView(m.view);
    } else if (m.t === 'here') waiting.get(m.id)?.();
  };
  if (ownComputer) {
    channel.postMessage({ t: 'open', id: ownComputer });
    addEventListener('pagehide', () => channel.postMessage({ t: 'closed', id: ownComputer }));
  } else {
    channel.postMessage({ t: 'who' });
  }
}

const separate = () => store.get('deskway.windows') === '1';

// Pressing computer `d`: its own window, when this device has them and this is not it.
export function openWindow(d, view) {
  if (!separate() || d.id === ownComputer) return false;
  const mine = opened.get(d.id);
  if (mine && !mine.closed) {
    mine.focus();
    channel?.postMessage({ t: 'front', id: d.id, view });
    return true;
  }
  if (channel && open.has(d.id)) {
    // Opened by another window: asked to come to the front. No answer, it is gone: opened anew
    // (still within the press, for the browser).
    const timer = setTimeout(() => { waiting.delete(d.id); open.delete(d.id); openNew(d, view); }, ANSWER_MS);
    waiting.set(d.id, () => {
      clearTimeout(timer);
      waiting.delete(d.id);
      // A browser may not let a window come forward by itself: then this one says where it is.
      setTimeout(() => { if (document.hasFocus()) gotIt(t('windows.openTitle', { name: d.name }), t('windows.openText')); }, ANSWER_MS);
    });
    channel.postMessage({ t: 'front', id: d.id, view });
    return true;
  }
  return openNew(d, view);
}

// False when the browser refused the window: the computer is then controlled here.
function openNew(d, view) {
  const url = new URL(location.pathname, location.origin);
  url.searchParams.set('computer', d.id);
  if (view !== 'desktop') url.searchParams.set('view', view);
  const w = window.open(url.href, `deskway-${d.id}`);
  if (w) opened.set(d.id, w);
  return !!w;
}
