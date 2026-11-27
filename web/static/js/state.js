// What the page knows (the account, its computers) and which view is showing. The pages that
// follow the server (list, detail, devices) and the account's own (account, members) register here
// and are redrawn by `render`; the other views ('login', 'waiting', 'setup', 'session') draw themselves.
import { socket } from './net.js';
import { dialogOpen } from './dialogs.js';
import { Signal } from './proto.js';

export const state = {
  me: null,
  devices: [],
  approvals: [],
  thumbs: {},
  // Thumbnails per display, keyed "<computer>|<display>".
  screenThumbs: {},
  view: 'login',
  deviceId: null,
  // False until the server sends the first list: before that, "no computers" would be a lie.
  devicesLoaded: false,
  // The signaling socket dropped and has not come back for a moment: what is shown may be stale.
  socketDown: false,
  // The server's build when this page loaded, and whether a newer one is there.
  build: null,
  updateReady: false,
  // A computer's AI activity is open: closed again on every change of view, so entering a
  // computer's page asks it for nothing until the person opens the record (views/audit.js).
  auditOpen: false,
};

const pages = {};

// `draw()` draws the page `name` into the app root.
export function registerPage(name, draw) { pages[name] = draw; }

export const device = () => state.devices.find((d) => d.id === state.deviceId);

// The list and a computer's page show computers and thumbnails as the server sends them.
export const followsServer = () => state.view === 'list' || state.view === 'detail';

// Ask for previews and vitals when a page that shows computers opens.
export function updateWatch() {
  socket.sendNow({ type: Signal.WATCH, on: followsServer() });
}

// Every change of view goes through here; `deviceId` is the computer it is about, if any.
export function setView(view, deviceId) {
  // Presses on the page being left are not this page's.
  down.clear();
  state.view = view;
  state.deviceId = deviceId ?? state.deviceId;
  state.auditOpen = false;
  updateWatch();
  render();
}

export function render() {
  if (pages[state.view]) pages[state.view]();
}

// The remote session open on this page, if any. A closed one is gone.
let current = null;
export const currentSession = () => (current && !current.closed ? current : null);
export function setCurrentSession(s) { current = s; }

export function endCurrentSession(s) {
  if (current !== s) return false;
  current = null;
  return true;
}

// Fingers and buttons down on the page, by pointer. A page rebuilt now would take the control
// from under them and the tap would do nothing, so updates from the server wait until they lift;
// at most HOLD_MAX, then the page is drawn anyway: a lift that never arrives (the element went
// away under the finger, as when a session ends) must not leave the page showing what was.
const down = new Set();
const HOLD_MAX = 2000;
let renderWaiting = false;
let holdTimer = null;
document.addEventListener('pointerdown', (e) => down.add(e.pointerId), true);
for (const k of ['pointerup', 'pointercancel']) {
  document.addEventListener(k, (e) => {
    down.delete(e.pointerId);
    // After the click that follows (a phone sends it a little later), so that click still lands
    // on the control it was meant for.
    if (!down.size && renderWaiting) setTimeout(renderFromServer, 300);
  }, true);
}

// Redraws the open page with what the server just sent; My devices and Add an AI client (its list of
// computers) not while a dialog is open on them.
export function renderFromServer() {
  if (down.size) {
    if (!renderWaiting) {
      renderWaiting = true;
      holdTimer = setTimeout(() => { down.clear(); renderFromServer(); }, HOLD_MAX);
    }
    return;
  }
  clearTimeout(holdTimer);
  renderWaiting = false;
  if (state.view === 'devices' || state.view === 'aiclient') { if (!dialogOpen()) render(); }
  else if (followsServer()) render();
}
