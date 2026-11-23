// Who holds the computer and the one question pressing its picture asks (docs/session-lifecycle.md,
// "Who controls"): `h.b` is the same device as `h.a` (another tab of the same browser), so it
// asks the device question (A5, D1) and the first tab is told the session moved; taking back
// (B13), back ending only its own session; the account question and the connecting wording
// (A6, A8, D2, D3) from the device list's controller; pressing while the server is unreachable
// (A2); the computer this browser runs on;
// disconnecting; each computer in a window of its own.
import { sleep, SCREENS, TAKE_OVER, click, centre, t, q, fixed } from './harness.mjs';

const dialogTitle = `document.querySelector('.dialog h2')?.textContent || ''`;
const hasButton = (label) => `[...document.querySelectorAll('.dialog button')].some((x) => x.textContent === ${JSON.stringify(label)})`;

export const area = {
  name: 'takeover',
  needs: ['auth'],
  async run(h) {
    const a = h.a;
    await h.sessionOn(a);
    const b = await h.secondPage();
    await b.v(`d.show('list')`);
    await b.until(`document.querySelector('.card .status')?.textContent.includes(${JSON.stringify(fixed('status.controlledBy'))})`, 'card shows the controller');
    h.check('another tab sees who is controlling', true, await b.eval(`document.querySelector('.card .status').textContent`));
    await b.eval(`document.querySelector('.card-open').click()`);
    await b.until(SCREENS, 'second detail page');

    // A5 / D1: this device holds it (the other tab): the device question.
    await b.eval(TAKE_OVER);
    await b.until(`!!document.querySelector('.dialog')`, 'takeover dialog');
    h.check('A5 the same device holding it asks the device question', await b.eval(`${dialogTitle}.includes(${q('force.deviceTitle')}) && ${hasButton(t('force.deviceGo'))}`), await b.eval(dialogTitle));
    await b.shot('takeover-wide', 1440, 900);
    await b.shot('takeover-portrait', 390, 844);
    await b.metrics(1440, 900);
    await b.clickText('.dialog button', t('common.cancel'));
    h.check('cancel keeps the first session', await a.v(`!!s && s.info().connection === 'connected'`));

    // A stale computer list can miss the holder. Deliver the host's busy reply once the
    // attempted connection has already moved the dialog layer into full screen.
    await b.send('Page.bringToFront');
    await b.send('Runtime.evaluate', { userGesture: true, expression: `(() => {
      const d = window.__deskway;
      d.interceptSignals((m) => {
        if (m.type !== 'bye' || m.reason !== 'busy') return m;
        window.__busyReply = m;
        return { type: 'ignored' };
      });
      localStorage.setItem('deskway.autofull', '1');
      d.serverSays({ type: 'devices', devices: d.devices().map((x) => ({ ...x, controller: undefined })) });
      ${TAKE_OVER};
    })()` });
    await b.until(`window.__busyReply && document.fullscreenElement?.contains(document.getElementById('dialog-root'))`, 'full screen before the busy reply');
    await b.v(`(d.interceptSignals(null), d.serverSays(window.__busyReply))`);
    await b.eval(`localStorage.setItem('deskway.autofull', '0')`);
    await b.until(`!document.fullscreenElement && ${hasButton(t('force.deviceGo'))}`, 'takeover dialog after the busy reply');
    await b.clickText('.dialog button', t('force.deviceGo'));
    await a.until(`${dialogTitle}.includes(${q('session.takenHereTitle')})`, 'first tab told the session moved');
    h.check('D1 confirmed: the first tab sees this device started remote in another window', true, await a.eval(`document.querySelector('.dialog p').textContent`));
    await a.shot('taken-over-wide', 1440, 900);
    await a.metrics(1440, 900);
    await b.until(`!!s && s.info().connection === 'connected' && s.info().w > 0`, 'second tab video after takeover');
    // The taker keeps the picture: the old session's cleanup must not stop video it no longer owns.
    const before = await b.v(`s.stats().then((x) => x.decoded)`);
    await sleep(4000);
    const later = await b.v(`s.stats().then((x) => x.decoded)`);
    // The sharper picture sends at most 2 frames a second, and only as the screen changes: a few
    // new ones show the video goes on.
    h.check('the taker keeps receiving frames after the takeover', later >= before + 3, `${before} -> ${later} frames in 4 s`);
    h.check('the tab taken over does not reconnect by itself', await a.v(`!s && document.querySelectorAll('.dialog').length === 1`) && await b.v(`!!s && s.info().state === 'active'`));

    // B13: "Take back" takes it back at once; the other tab is told.
    await a.send('Page.bringToFront');
    await a.clickText('.dialog button', t('session.takeBack'));
    await a.until(`!!s && s.info().state === 'active' && s.info().connection === 'connected'`, 'taken back', 30000);
    await b.until(`!s && ${dialogTitle}.includes(${q('session.takenHereTitle')})`, 'the other tab told');
    h.check('B13 Take back takes the computer back, the other tab is told', true);
    // The history entry of the session that was taken left meanwhile: the new one stays.
    await sleep(2000);
    h.check('back after take-back keeps the new session', await a.v(`!!s && s.info().state === 'active'`));
    // Going back ends exactly this session.
    await a.eval(`history.back()`);
    await a.until(`!s && d.view() === 'detail'`, 'back ends the session');
    h.check('B1 going back ends the session', true);
    await b.send('Page.bringToFront');
    await b.clickText('.dialog button', t('common.back'));
    await b.v(`d.show('detail', d.devices()[0].id)`);
    await b.until(`d.devices()[0] && !d.devices()[0].controller`, 'the computer is free');

    // A6, A8 / D2 / D3: the question for whoever holds it, from the device list's controller.
    const asked = async (controller, label) => {
      await b.v(`d.serverSays({ type: 'devices', devices: d.devices().map((x) => ({ ...x, controller: ${JSON.stringify(controller)} })) })`);
      await b.until(SCREENS, 'picture');
      await b.eval(TAKE_OVER);
      await b.until(`!!document.querySelector('.dialog')`, label);
      const got = { title: await b.eval(dialogTitle), buttons: await b.eval(`[...document.querySelectorAll('.dialog button')].map((x) => x.textContent)`) };
      await b.clickText('.dialog button', t('common.cancel'));
      return got;
    };
    let question = await asked({ device: false, name: '小林的手机', connecting: false }, 'account question');
    h.check('A6 another device of this account: "your … is controlling", Take over', question.title === t('force.account', { name: '小林的手机' }) && question.buttons.includes(t('force.accountGo')), JSON.stringify(question));
    question = await asked({ device: false, name: '小林的手机', connecting: true }, 'account question, connecting');
    h.check('A8 still connecting says so', question.title === t('force.accountConnecting', { name: '小林的手机' }), JSON.stringify(question));
    // A3: a computer older than this page understands: the update notice, no session.
    await b.v(`d.serverSays({ type: 'devices', devices: d.devices().map((x) => ({ ...x, controller: undefined, version: 2 })) })`);
    await b.until(SCREENS, 'picture');
    await b.eval(TAKE_OVER);
    await b.until(`!!document.querySelector('.dialog')`, 'update notice');
    h.check('A3 a computer too old: the update notice, no session', await b.eval(`${dialogTitle} === ${q('update.computerTitle')}`) && await b.v(`!s`), await b.eval(dialogTitle));
    await b.clickText('.dialog button', t('common.gotIt'));
    await b.v(`d.serverSays({ type: 'devices', devices: d.devices().map((x) => ({ ...x, controller: undefined, version: 3 })) })`);

    // A2: the page knows its socket is down: pressing says so at once and starts nothing.
    await b.v(`d.dropSignaling()`);
    await b.eval(TAKE_OVER);
    await b.until(`!!document.querySelector('.dialog')`, 'unreachable said');
    h.check('A2 pressing while the server is unreachable says so at once; no session starts', await b.eval(`${dialogTitle} === ${q('session.unreachableTitle')}`) && await b.v(`!s`), await b.eval(dialogTitle));
    await b.clickText('.dialog button', t('common.gotIt'));
    await b.until(`d.socketOpen()`, 'socket back', 20000);

    // The computer this browser runs on, which the test computer is, as the page finds by itself
    // once it says where it runs (here.js): the list and its page say "This computer", and pressing it asks
    // first. Cancelled, nothing starts; confirmed, it connects as always.
    await b.v(`d.locating(true)`);
    await b.until(`d.devices()[0]?.here === true`, 'the computer this browser runs on', 15000);
    await b.v(`d.show('list')`);
    await b.until(`document.querySelector('.card .card-name .tag')?.textContent === ${q('here.tag')}`, 'the list says This computer');
    h.check('the list marks the computer this browser runs on: This computer', true);
    await b.v(`d.show('detail', d.devices()[0].id)`);
    await b.until(SCREENS, 'its page');
    h.check('its page says This computer too', await b.eval(`document.querySelector('.heading .tag')?.textContent === ${q('here.tag')}`));
    await b.eval(TAKE_OVER);
    await b.until(`!!document.querySelector('.dialog')`, 'asked first');
    h.check('pressing the computer this browser runs on asks first', await b.eval(`${dialogTitle} === ${q('here.confirmTitle')} && ${hasButton(t('here.confirmGo'))}`), await b.eval(dialogTitle));
    await b.shot('here-confirm', 390, 844);
    await b.metrics(1440, 900);
    await b.clickText('.dialog button', t('common.cancel'));
    await sleep(300);
    h.check('cancelled, nothing starts', await b.v(`!s && !document.querySelector('.dialog')`));
    await b.eval(TAKE_OVER);
    await b.until(`!!document.querySelector('.dialog')`, 'asked again');
    await b.clickText('.dialog button', t('here.confirmGo'));
    await b.until(`!!s && s.info().connection === 'connected'`, 'session for Disconnect', 30000);
    h.check('confirmed, it connects as always', true);
    await b.v(`d.locating(false)`);
    await b.metrics(1440, 900);
    await b.v(`s.hideToolbar()`);
    await b.clickText('.toolbar button', t('tb.disconnect'));
    await b.until(`document.querySelector('.dialog h2')?.textContent.includes(${JSON.stringify(fixed('leave.title'))})`, 'disconnect asks first');
    await b.clickText('.dialog button', t('common.cancel'));
    await sleep(300);
    h.check('disconnect asks first, and cancel keeps the session', await b.v(`!!document.querySelector('.session') && s.info().connection === 'connected'`));
    await b.clickText('.toolbar button', t('tb.disconnect'));
    await b.until(`!!document.querySelector('.dialog')`, 'disconnect dialog again');
    await b.clickText('.dialog button', t('tb.disconnect'), { exact: true });
    await b.until(`!document.querySelector('.session') && ${SCREENS}`, 'back on detail');
    h.check('disconnect returns to the detail page', true);
    await b.until(`!document.querySelector('.status')?.textContent.includes(${JSON.stringify(fixed('status.controlledBy'))})`, 'controller cleared');
    h.check('controller clears after disconnect', true);

    // "Each computer in its own window": a press (a real one: opening a window needs it) opens the
    // computer's own window and leaves this page as it is; pressing again brings that window
    // back instead of opening another.
    const windows = async () => (await (await fetch(`${h.devtools}/json/list`)).json()).filter((x) => x.type === 'page' && x.url.includes('?computer='));
    const id = await b.v(`d.devices()[0].id`);
    await b.eval(`localStorage.setItem('deskway.windows', '1')`);
    await click(b, await centre(b, '.display-tile, .preview.clickable'));
    for (let i = 0; i < 30 && !(await windows()).length; i++) await sleep(200);
    const opened = await windows();
    h.check('with each computer in its own window, pressing it opens its window, and this page stays', opened.length === 1 && opened[0].url.endsWith(`?computer=${id}`) && await b.v(`!s && !document.querySelector('.session')`), JSON.stringify(opened.map((x) => x.url)));
    await click(b, await centre(b, '.display-tile, .preview.clickable'));
    await sleep(1000);
    h.check('pressing it again brings that window back instead of opening another', (await windows()).length === 1 && await b.v(`!s`));
    for (const w of await windows()) await fetch(`${h.devtools}/json/close/${w.id}`);
    await b.eval(`localStorage.removeItem('deskway.windows')`);
    await b.until(`!d.devices()[0].controller`, 'the computer is free again', 30000);
  },
};
