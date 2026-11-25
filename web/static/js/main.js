// Startup: wires the pages, the server's messages and the page's own events together, then
// loads the language and finds out who is signed in.
import { store } from './env.js';
import { pickLanguage, setLanguage } from './i18n.js';
import { api, onUnauthorized, onMustChoose, socket, setIce } from './net.js';
import { state, registerPage, render, renderFromServer, updateWatch, currentSession, followsServer } from './state.js';
import { onDialogClosed } from './dialogs.js';
import { start, showLogin, showChoose, showUnreachable } from './views/login.js';
import { renderList, onThumb } from './views/list.js';
import { renderDetail } from './views/detail.js';
import { renderDevices } from './views/devices.js';
import { renderAccount } from './views/account.js';
import { renderMembers } from './views/members.js';
import { renderRelays } from './views/relays.js';
import { renderAiClient } from './views/ai-client.js';
import { showNextApproval } from './views/approvals.js';
import { onPairReply, offerQrPair, takePairLink } from './views/pairing.js';
import { openSession, startOwnWindow } from './views/takeover.js';
import { startPwa } from './pwa.js';
import { locate } from './here.js';
import { onAudit } from './views/audit.js';
import { installAppFeel } from './app-feel.js';
import { PROTOCOL_VERSION, Signal } from './proto.js';

installAppFeel();
startPwa(() => { if (!currentSession() && state.view !== 'login') render(); });
registerPage('list', renderList);
registerPage('detail', renderDetail);
registerPage('devices', renderDevices);
registerPage('account', renderAccount);
registerPage('members', renderMembers);
registerPage('relays', renderRelays);
registerPage('aiclient', renderAiClient);
onUnauthorized(showLogin);
onMustChoose(showChoose);
// A request to approve waits while another dialog is open.
onDialogClosed(showNextApproval);

socket.onUp = () => {
  if (state.socketDown) {
    state.socketDown = false;
    if (followsServer()) render();
  }
  updateWatch();
  locate();
  // A session outlives the socket: its connection claims it again on the new one.
  currentSession()?.connection.onSocketUp();
};
socket.onDown = () => {
  state.socketDown = true;
  currentSession()?.connection.onSocketDown();
  if (followsServer()) render();
  // The server may have closed it because this browser was signed out (removed, or its account
  // removed): asking who is signed in shows the sign-in page then (api's 401), which also ends
  // a session. Unreachable, it stays as it is.
  api('GET', '/api/me').catch(() => {});
};
socket.onMessage = (m) => {
  switch (m.type) {
    case Signal.HELLO:
      // A server newer than this page (another protocol, or another build after a deploy): the
      // pages say so, and reloading takes the new one. Never in the middle of a session.
      if (m.build && !state.build) state.build = m.build;
      if (m.version > PROTOCOL_VERSION || (m.build && m.build !== state.build)) {
        state.updateReady = true;
        if (!currentSession()) render();
      }
      break;
    case Signal.DEVICES:
      state.devices = m.devices || [];
      state.devicesLoaded = true;
      currentSession()?.toolbar?.showAi(state.devices.find((d) => d.id === currentSession().device.id));
      offerQrPair((d) => openSession(d, false));
      renderFromServer();
      startOwnWindow();
      break;
    case Signal.APPROVALS:
      state.approvals = m.approvals || [];
      // Add an AI client shows an AI client's request itself.
      if (state.view === 'aiclient') render();
      showNextApproval();
      break;
    case Signal.ICE:
      setIce(m.ice || []);
      break;
    case Signal.THUMB:
      onThumb(m);
      break;
    case Signal.AUDIT:
      onAudit(m);
      break;
    case Signal.PAIRED:
    case Signal.NONCE:
    case Signal.ATTACHED:
    case Signal.PRESENCE:
    case Signal.ANSWER:
    case Signal.CANDIDATE:
    case Signal.BYE: {
      if (onPairReply(m)) break;
      const s = currentSession();
      if (s && m.session === s.id) s.connection.onSignal(m);
      break;
    }
  }
};

document.addEventListener('fullscreenchange', showNextApproval);
// Back after a while in the background, or the network is back: the socket may have died without
// the browser knowing yet (net.js, renew).
const AWAY_MS = 10000;
let hiddenAt = 0;
document.addEventListener('visibilitychange', () => {
  if (document.hidden) hiddenAt = Date.now();
  else if (hiddenAt && Date.now() - hiddenAt >= AWAY_MS) socket.renew();
});
window.addEventListener('online', () => socket.renew());
// Leaving the page (closing it, or reloading it) ends the session, telling the computer; a
// reload keeps no resume token. Safari may keep the page frozen in memory instead of closing
// it; a page that comes back from that freeze starts fresh.
window.addEventListener('pagehide', (e) => {
  currentSession()?.leave();
  if (e.persisted) socket.close();
});
window.addEventListener('pageshow', (e) => { if (e.persisted) location.reload(); });

// The e2e tests' surface, loaded only when they ask for it (test-api.js).
if (store.get('deskway.test') === '1') (await import('./test-api.js')).installTestApi();
takePairLink();
// The texts first, so the first screen is already in the right language.
setLanguage(pickLanguage()).then(start).catch(() => showUnreachable());
