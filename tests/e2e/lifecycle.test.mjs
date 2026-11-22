// The session's life on its peer connection: the back gesture ends it; the computer's real
// caps shape the page; an ICE restart and a lost signaling socket keep the same session; and
// after a server restart (with --server-exe and --server-state) the session goes on, is
// attached again, and ends exactly once, with the right words, by "Disconnect", by the person at the
// computer, or by a takeover, never reconnecting by itself.
import { sleep, ALL_CAPS, t } from './harness.mjs';

export const area = {
  name: 'lifecycle',
  needs: ['auth'],
  async run(h) {
    const a = h.a;
    const b = await h.secondPage();
    await h.freeComputer(b);
    const controlled = () => b.v(`!!d.devices()[0]?.controller`);
    const freeAgain = async () => {
      for (let i = 0; i < 50; i++) { if (!(await controlled())) return true; await sleep(200); }
      return false;
    };
    const startB = async (force = false) => {
      await b.send('Page.bringToFront');
      await b.v(`d.open(d.devices()[0].id, { force: ${force} })`);
      await b.until(`!!s && s.info().state === 'active' && s.info().connection === 'connected' && !!s.info().caps`, 'session up', 30000);
      await b.until(`!!d.devices()[0]?.controller`, 'computer shows it is controlled');
    };
    // The page ended its session once, shows `title`, and does not start another for a while.
    const endedOnce = async (p, title, label) => {
      await p.until(`!s && document.querySelectorAll('.dialog').length === 1 && document.querySelector('.dialog h2').textContent.includes(${JSON.stringify(title)})`, label, 20000);
      await sleep(4000);
      return p.v(`!s && document.querySelectorAll('.dialog').length === 1`);
    };
    const closeDialog = (p) => p.eval(`document.querySelector('.dialog button')?.click()`);

    await startB();
    await b.eval(`history.back()`);
    h.check('lifecycle: the back gesture ends the session', await freeAgain() && await b.v(`d.view() === 'detail' && !s`));

    // The test computer is view-only: without the caps double, its hello has no input or files,
    // and the page offers neither.
    await b.v(`d.hostCaps(null)`);
    await startB();
    const caps = await b.v(`s.info().caps`);
    h.check('the page takes the computer\'s caps from its hello: a view-only computer gets no input controls', !caps.includes('input') && !caps.includes('files') && caps.includes('displays') && await b.v(`s.el.kbdBtn.hidden && s.el.modeBtn.hidden`), caps.join(','));
    await b.v(`s.leave()`);
    await b.v(`d.hostCaps(${JSON.stringify(ALL_CAPS)})`);
    await freeAgain();

    // An ICE restart (what a network blip triggers) renegotiates on the same peer connection:
    // the same session, new ICE credentials, the picture goes on.
    await startB();
    await b.until(`s.info().w > 0`, 'video');
    const id = await b.v(`s.info().id`);
    const ufrag = await b.v(`s.info().ufrag`);
    const frames = await b.v(`s.stats().then((x) => x.decoded)`);
    await b.v(`s.restartIce()`);
    await b.until(`s.info().ufrag !== ${JSON.stringify(ufrag)} && s.info().connection === 'connected'`, 'ICE restarted', 20000);
    // A still screen sends nothing new: a keyframe asked for shows the picture still flows.
    await b.v(`s.keyframe()`);
    await sleep(1500);
    const after = await b.v(`({ id: s.info().id, state: s.info().state, decoded: 0 })`);
    after.decoded = await b.v(`s.stats().then((x) => x.decoded)`);
    h.check('an ICE restart keeps the same session and its picture', after.id === id && after.state === 'active' && after.decoded > frames, `${frames} -> ${after.decoded} frames`);

    // The signaling socket drops (a network change): the session goes on and attaches again.
    await b.v(`d.dropSignaling()`);
    await b.until(`d.socketOpen()`, 'socket back', 20000);
    // Attached again: its signaling goes through the new socket (an ICE restart is answered).
    const ufrag1 = await b.v(`s.info().ufrag`);
    await b.v(`s.restartIce()`);
    await b.until(`s.info().ufrag !== ${JSON.stringify(ufrag1)} && s.info().connection === 'connected'`, 'ICE restart after attaching again', 20000);
    h.check('losing the signaling socket ends nothing: the session attaches again', await b.v(`s.info().id === ${JSON.stringify(id)} && s.info().state === 'active'`));
    await b.v(`s.leave()`);
    await freeAgain();

    if (!(h.opts.serverExe && h.opts.serverState)) return;

    // A12: the server restarts while connecting (held before the offer: no nonce reached the
    // page). The page asks its challenge again on the new server, never attach; the computer
    // gives the same nonce, and the session carries on, well before the deadline.
    await b.v(`(window.__restarted = false, d.interceptSignals((m) => (m.type === 'nonce' && !window.__restarted ? { type: 'ignored' } : m)))`);
    await b.v(`d.open(d.devices()[0].id)`);
    await b.until(`!!s && s.info().state === 'connecting'`, 'connecting');
    const restarted = Date.now();
    await b.v(`window.__restarted = true`);
    await h.restartServer();
    await b.until(`!!s && s.info().state === 'active'`, 'connected after the server restart', 25000);
    h.check('A12 a server restart while connecting: the challenge again on the new server, the session carries on', Date.now() - restarted < 25000, `${Date.now() - restarted} ms`);
    await b.v(`d.interceptSignals(null)`);
    await b.v(`s.leave()`);
    await b.until(`d.devices()[0] && !d.devices()[0].controller`, 'the session left', 5000);
    const restart = async (label) => {
      await h.restartServer();
      await b.until(`d.socketOpen() && d.devices().length > 0 && d.devices()[0].online && !!d.devices()[0].controller && !!s && s.info().state === 'active'`, label, 30000);
    };

    // A server restart mid-session: the session goes on, is attached again (signaling for it
    // works: an ICE restart goes through the new server), and "Disconnect" ends it at once.
    await startB();
    await restart('server back, the session attached again');
    h.check('a server restart during a session: the session goes on and is attached again', await b.v(`s.info().state === 'active' && s.info().connection === 'connected'`));
    const ufrag2 = await b.v(`s.info().ufrag`);
    await b.v(`s.restartIce()`);
    await b.until(`s.info().ufrag !== ${JSON.stringify(ufrag2)} && s.info().connection === 'connected'`, 'ICE restart through the new server', 20000);
    h.check('…its signaling is routed again: an ICE restart goes through the new server', true);
    const left = Date.now();
    await b.v(`s.leave()`);
    await b.until(`d.devices()[0] && !d.devices()[0].controller`, '"in control" cleared after Disconnect', 3000);
    h.check('…and Disconnect after it ends the session at the computer at once', true, `${Date.now() - left} ms`);
    await sleep(3000);
    h.check('…and it does not come back by itself', await b.v(`!s && !d.devices()[0].controller`));

    // After a restart, the person at the computer ends it from the panel: told once, no reconnect.
    await startB();
    await restart('server back before the panel ends it');
    const panel = h.panelPipe();
    await sleep(300);
    panel.send('Disconnect');
    const endedOk = await endedOnce(b, t('session.endedTitle'), 'told the computer ended it');
    panel.close();
    h.check('after a server restart, Disconnect at the computer ends the session once, says so, and it does not reconnect', endedOk && await freeAgain());
    await closeDialog(b);

    // After a restart, another browser takes over: the first is told once and stays off.
    await startB();
    await restart('server back before the takeover');
    await a.send('Page.bringToFront');
    await a.v(`d.show('list')`);
    await a.until(`d.devices().length > 0 && !!d.devices()[0].controller`, 'the other browser sees the controller');
    await a.v(`d.open(d.devices()[0].id, { force: true })`);
    await a.until(`!!s && s.info().state === 'active'`, 'the taker connected', 30000);
    const takenOk = await endedOnce(b, t('session.takenHereTitle'), 'the first tab told the session moved');
    h.check('after a server restart, a takeover from another tab of this device ends the first session once, says so, and it stays off', takenOk && await a.v(`!!s && s.info().state === 'active'`));
    await closeDialog(b);
    await a.v(`s.leave()`);
    await freeAgain();
  },
};
