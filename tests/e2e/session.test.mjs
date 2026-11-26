// A session from the first device: the computer's page with its displays, connecting signed
// with the device key, reconnecting with the resume token, switching displays, the pointer,
// the "Keyboard" button, full screen and the toolbar there, and the video's numbers.
// Leaves the session open on `h.a`.
import { sleep, SCREENS, click, t, q, fixed, lit } from './harness.mjs';
import { MIN_VERSION } from '../../web/static/js/proto.js';

// The video's size follows what the page shows and decodes (`view`): a phone gets no more than
// its screen, a window that grows gets more, a 4K-sized window gets the display at up to the
// encoder's highest H.264 level. And a display that comes back is waited for frame by frame.
async function viewSizes(h, a) {
  const size = `s.info().w + 'x' + s.info().h`;
  const fits = `(s.info().view && s.info().w <= s.info().view.w && s.info().h <= s.info().view.h)`;
  const display = await a.v(`(() => { const i = s.info(); return i.displays.find((x) => x.id === i.display); })()`);
  const aspect = (w, hh) => Math.abs((w / hh) / (display.w / display.h) - 1) < 0.01;
  await a.metrics(1440, 900);
  await a.until(`${fits} && s.info().view.w === Math.round(s.el.stage.getBoundingClientRect().width)`, 'video within a 1440x900 window', 10000);
  const wide = await a.v(`s.info()`);
  h.check('view: the page tells the host its stage in device pixels and what it decodes smoothly (MediaCapabilities)', wide.view.maxW >= 1280 && wide.view.fps > 0, JSON.stringify(wide.view));
  h.check('view: the video is no larger than the window and keeps the display\'s shape', aspect(wide.w, wide.h), `${wide.w}x${wide.h} for ${display.w}x${display.h} in ${wide.view.w}x${wide.view.h}`);

  await a.metrics(390, 844, true);
  await a.until(`${fits} && s.info().view.w === 1170 && ${size} !== ${JSON.stringify(`${wide.w}x${wide.h}`)}`, 'video sized for a phone', 10000);
  const phone = await a.v(`s.info()`);
  h.check('view: a phone-sized viewer (390x844 at 3x) gets a stream no larger than its screen', phone.w <= 1170 && phone.h <= 2532 && aspect(phone.w, phone.h), `${phone.w}x${phone.h}`);

  await a.metrics(1440, 900);
  await a.until(`${size} === ${JSON.stringify(`${wide.w}x${wide.h}`)}`, 'video back at the window size', 10000);
  h.check('view: making the window larger again makes the stream larger again', true, `${phone.w}x${phone.h} -> ${wide.w}x${wide.h}`);

  // A pixel-ratio change alone (another monitor) is covered by tests/unit/constraints.test.mjs:
  // DevTools' emulated ratio updates media queries without dispatching their change event.

  // A 4K-sized window: the display at its own size, or as near as this browser's decode box
  // (turned like the display) and H.264 level 5.2 allow.
  await a.metrics(3840, 2560);
  const [long, short] = [Math.max(wide.view.maxW, wide.view.maxH), Math.min(wide.view.maxW, wide.view.maxH)];
  const [bw, bh] = display.w >= display.h ? [long, short] : [short, long];
  const want = Math.floor(display.w * Math.min(1, 3840 / display.w, 2560 / display.h, bw / display.w, bh / display.h));
  await a.until(`s.info().w >= ${Math.floor(want * 0.95)}`, 'video at 4K', 15000);
  const big = await a.v(`s.info()`);
  const mbs = Math.ceil(big.w / 16) * Math.ceil(big.h / 16);
  h.check('view: a 4K-sized window gets the 4K display, within the decode box and level 5.2 (36864 macroblocks)', mbs <= 36864 && big.w <= bw && big.h <= bh && aspect(big.w, big.h), `${big.w}x${big.h} (${mbs} MBs) for ${display.w}x${display.h}, box ${bw}x${bh}`);
  const s1 = await a.v(`s.stats()`);
  await sleep(1500);
  const s2 = await a.v(`s.stats()`);
  h.check('view: 4K frames keep being decoded', s2.decoded >= s1.decoded && s2.w === big.w, `${s1.decoded} -> ${s2.decoded}`);
  await a.metrics(1440, 900);
  await a.until(`${size} === ${JSON.stringify(`${wide.w}x${wide.h}`)}`, 'video back at the window size', 10000);

  // The display goes and comes back: input waits for a newly presented frame (the keyframe
  // asked for), seen through requestVideoFrameCallback.
  await a.v(`s.fromHost({ t: 'capture', v: 'no-display' })`);
  h.check('capture: no display blocks input', await a.v(`s.info().capture === 'no-display'`));
  await a.v(`s.fromHost({ t: 'capture', v: 'ready' })`);
  await a.until(`s.info().capture === null`, 'a new frame after the display came back', 8000);
  h.check('capture: input resumes with the next frame presented', true);
}

export const area = {
  name: 'session',
  needs: ['auth'],
  async run(h) {
    const a = h.a;
    await h.freeComputer(a);
    await a.v(`d.show('list')`);
    await a.until(`!!document.querySelector('.card')`, 'computer list');
    await a.eval(`document.querySelector('.card-open').click()`);
    await a.until(SCREENS, 'detail page');
    h.check('Windows offers no terminal entry',
      await a.eval(`![...document.querySelectorAll('[data-page="detail"] button')].some((b) => b.textContent === ${lit(t('terminal.label'))})`));
    h.check('card opens detail, not a session', await a.eval(`!document.querySelector('.session')`));
    // This test computer has two displays: the detail page draws them.
    await a.until(`document.querySelectorAll('.display-tile').length === 2`, 'display layout on the detail page', 15000);
    await a.until(`[...document.querySelectorAll('.display-tile img')].every((i) => !i.hidden && i.naturalWidth > 0)`, 'a preview in every display', 15000);
    h.check('with two displays the detail page shows a preview of each, where it sits, instead of one picture', await a.eval(`!document.querySelector('.preview.clickable')`), await a.eval(`[...document.querySelectorAll('.display-label')].map((x) => x.textContent).join(' / ')`));
    await a.shot('detail-wide', 1440, 900);
    await a.shot('detail-portrait', 390, 844);
    await a.metrics(1440, 900);

    console.log(`      setup timing: ${await a.eval(`(async () => {
      const out = []; let t = performance.now();
      const lap = (k) => { out.push(k + '=' + Math.round(performance.now() - t)); t = performance.now(); };
      const pc = new RTCPeerConnection(); lap('newPC');
      pc.addTransceiver('video', { direction: 'recvonly' }); pc.createDataChannel('x'); lap('addTransceiver');
      await pc.createOffer(); lap('createOffer');
      pc.close();
      return out.join(' ');
    })()`)}`);
    // A computer that has never trusted a device takes the account's device list as it is, once:
    // this first device just works. Clicking the picture takes over.
    const t0 = Date.now();
    await a.eval(`[...document.querySelectorAll('.display-tile')].find((t) => t.textContent.includes(${JSON.stringify(fixed('display.tileMain'))})).click()`);
    await a.until(`!!s && !!s.info().connection`, 'peer connection created');
    await a.v(`s.startTrace()`);
    await a.until(`s && s.info().connection === 'connected'`, 'peer connection');
    const connectMs = Date.now() - t0;
    console.log(`      browser timeline: ${await a.v(`s.trace()`)}`);
    await a.until(`s.info().w > 0`, 'first decoded frame').catch(async (e) => {
      console.log(`      stats at failure: ${JSON.stringify(await a.v(`s.stats()`))}`);
      throw e;
    });
    h.check('video starts', true, `connected in ${connectMs} ms, first frame in ${Date.now() - t0} ms`);

    // The picture: sharper by default (the host is asked for 2 frames a second), switched from the
    // toolbar to smoother (24) and back, remembered in this browser, without reconnecting.
    const viewFps = `s.info().view && s.info().view.fps`;
    await a.until(`${viewFps} === 2`, 'the sharp picture asks for 2 frames a second');
    h.check('picture: sharper by default, the toolbar says Sharp', await a.v(`!localStorage.getItem('deskway.picture') && s.el.pictureBtn.textContent === ${q('tb.sharp')}`));
    await a.v(`s.el.pictureBtn.click()`);
    await a.until(`${viewFps} === 24`, 'smoother asks for 24');
    h.check('picture: Smooth from the toolbar asks for 24 frames a second at once, remembered, the session kept', await a.v(`localStorage.getItem('deskway.picture') === 'smooth' && s.el.pictureBtn.textContent === ${q('tb.smooth')} && s.info().connection === 'connected'`));
    await a.v(`s.el.pictureBtn.click()`);
    await a.until(`${viewFps} === 2`, 'back to sharper');

    // B23 picture in picture: nothing to press; the picture floats when the page goes to the
    // background (as the browser starts it) and is back in the page when the page is in front.
    h.check('picture in picture: no button for it in the toolbar', await a.eval(`![...document.querySelectorAll('.toolbar button')].some((b) => /picture.in.picture|mini window/i.test(b.textContent))`));
    if (await a.v(`document.pictureInPictureEnabled`)) {
      await a.send('Runtime.evaluate', { expression: `window.__deskway.session().pipEnter()`, userGesture: true });
      await a.until(`s.pipActive()`, 'the picture floats');
      h.check('picture in picture: floating, the session goes on', await a.v(`s.info().state === 'active'`));
      // Back in front.
      await a.eval(`document.dispatchEvent(new Event('visibilitychange'))`);
      await a.until(`!s.pipActive()`, 'the picture back in the page');
      h.check('picture in picture: back in front, the picture is in the page again', await a.v(`!document.pictureInPictureElement && s.info().state === 'active'`));
    }
    await a.until(`s.info().input === 'open'`, 'input channel');
    h.check('input channel opens', true);
    h.check('the session is signed with the browser\'s own device key, without any prompt', await a.v(`s.info().auth === 'dk'`));
    h.check('a new computer accepts the account\'s first device without scanning anything', true);

    // A new session to the same computer uses the resume token, without the device key.
    await a.until(`s.info().resumable`, 'resume token from the host');
    await a.v(`s.leave()`);
    await a.until(`!s && d.devices()[0] && !d.devices()[0].controller`, 'the computer is free');
    await a.v(`d.open(d.devices()[0].id)`);
    await a.until(`s && s.info().connection === 'connected' && s.info().input === 'open' && s.info().auth === 'rs'`, 'reconnected');
    h.check('connecting again uses the resume token', true);
    await a.until(`s.info().w > 0`, 'video after reconnecting');

    // Displays: the host lists them; switching changes the picture and is remembered.
    await a.until(`s.info().displays.length > 0 && !!s.info().display`, 'display list from the host');
    const displays = await a.v(`s.info().displays.map((x) => x.w + 'x' + x.h + (x.primary ? '*' : '')).join(' ')`);
    h.check('the host reports its displays', true, displays);
    h.check('the display button shows only with several displays', await a.v(`s.el.displayBtn.hidden === (s.info().displays.length < 2)`));
    if (await a.v(`s.info().displays.length > 1`)) {
      const first = await a.v(`s.info().display`);
      const size = `s.info().w + 'x' + s.info().h`;
      const before = await a.v(size);
      await a.v(`s.el.displayBtn.click()`);
      await a.until(`document.querySelectorAll('.display-btn').length === s.info().displays.length`, 'display panel');
      await a.shot('session-displays', 1440, 900);
      await a.metrics(1440, 900);
      await a.eval(`document.querySelector('.display-btn:not(.current)').click()`);
      await a.until(`s.info().display !== ${JSON.stringify(first)}`, 'host switched display');
      const target = await a.v(`(() => { const i = s.info(); const x = i.displays.find((y) => y.id === i.display); return x.w + 'x' + x.h; })()`);
      await a.until(`${size} !== ${JSON.stringify(before)} || ${JSON.stringify(target)} === ${JSON.stringify(before)}`, 'video follows the display', 8000);
      h.check('switching display changes the video', true, `${before} -> ${await a.v(size)} (display ${target})`);
      h.check('the choice is remembered for this computer', await a.v(`localStorage.getItem('deskway.display.' + s.info().device) === s.info().display`));
      await a.v(`s.chooseDisplay(${JSON.stringify(first)})`);
      await a.until(`s.info().display === ${JSON.stringify(first)} && ${size} === ${JSON.stringify(before)}`, 'back on the first display', 8000);
      h.check('switching back restores the first display', true);
    }
    await viewSizes(h, a);
    // Pointer: the host stops drawing it into the video and reports it instead.
    await a.until(`s.info().pointer.active`, 'pointer reports from the host', 5000).then(
      () => h.check('the host reports the pointer to the browser', true),
      () => h.check('the host reports the pointer to the browser', false));
    console.log(`      pointer shape received: ${await a.v(`s.info().pointer.shape || 'not yet (only sent when the pointer changes)'`)}`);
    await a.shot('session-wide', 1440, 900);
    h.check('a computer shows no floating keyboard by itself: its physical keys go straight through', await a.eval(`!document.querySelector('.keypad')`));
    await a.clickText('.toolbar button', t('tb.keyboard'));
    h.check('the "Keyboard" button opens the floating keyboard on a computer too', await a.eval(`!!document.querySelector('.keypad')`));
    await a.shot('session-keyboard-wide', 1440, 900);
    await a.clickText('.toolbar button', t('tb.keyboard'));
    await a.metrics(1440, 900);

    // ---- full screen ----
    // requestFullscreen needs a user gesture, which only this evaluate flag can supply here.
    const fullClick = `[...document.querySelectorAll('.toolbar button')].find(b => b.textContent.includes(${q('tb.full')})).click()`;
    await a.send('Runtime.evaluate', { expression: fullClick, userGesture: true, awaitPromise: true });
    await a.until(`document.fullscreenElement && s.info().full`, 'fullscreen on');
    h.check('fullscreen: the screen fills the whole page', await a.eval(`(() => { const s = document.querySelector('.stage').getBoundingClientRect(); return s.width === innerWidth && s.height === innerHeight; })()`));
    h.check('fullscreen: session stays connected', await a.v(`s.info().connection === 'connected' && s.info().w > 0`));
    // In the middle of the top edge, as in a window.
    const middle = (sel) => `(() => { const r = document.querySelector('${sel}').getBoundingClientRect(); return Math.abs(r.left + r.width / 2 - innerWidth / 2) < 1 && r.top >= 0 && r.top < 10; })()`;
    const box = (sel) => a.eval(`JSON.stringify(document.querySelector('${sel}').getBoundingClientRect())`);
    h.check('fullscreen: the toolbar\'s tab is in the middle of the top edge', await a.eval(middle('.toolbar-tab')), await box('.toolbar-tab'));
    await a.eval(`document.querySelector('.toolbar-tab').click()`);
    await a.until(`document.querySelector('.toolbar.shown')`, 'toolbar slides in');
    await sleep(250);
    await a.shot('session-fullscreen', 1440, 900);
    h.check('fullscreen: the toolbar can be brought back, in the middle of the top edge', await a.eval(middle('.toolbar')), await box('.toolbar'));
    const toolbarAway = `(() => { const t = document.querySelector('.toolbar'); return !t.classList.contains('shown') && t.getBoundingClientRect().bottom <= 0; })()`;
    // Using a button (it keeps focus) must not pin the toolbar over the screen.
    await a.clickText('.toolbar button', t('clip.name'));
    await sleep(300);
    h.check('fullscreen: panels open in the middle too, under the toolbar', await a.v(`(() => { const r = s.el.panels.getBoundingClientRect(), t = document.querySelector('.toolbar').getBoundingClientRect(); return r.width > 0 && Math.abs(r.left + r.width / 2 - innerWidth / 2) < 1 && r.top >= t.bottom; })()`), await box('.panels'));
    h.check('fullscreen: the toolbar stays while being used', await a.eval(`document.querySelector('.toolbar').classList.contains('shown')`));
    await a.until(toolbarAway, 'toolbar hides after a button is used', 4000);
    h.check('fullscreen: the toolbar hides by itself after a button is used', true);
    await a.clickText('.toolbar button', t('clip.name'));
    await a.v(`s.showToolbar()`);
    await a.until(`document.querySelector('.toolbar.shown')`, 'toolbar shown again');
    // A real click on the remote screen (the test host is view only, so nothing is clicked there).
    await click(a, await a.v(`(() => { const r = s.el.stage.getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height * 0.7]; })()`));
    await sleep(400);
    h.check('fullscreen: touching the screen hides the toolbar at once', await a.eval(toolbarAway));
    await a.v(`s.showToolbar()`);
    await a.until(toolbarAway, 'toolbar hides when left alone', 5000);
    h.check('fullscreen: the toolbar hides by itself when left alone', true);
    await a.v(`s.confirmLeave()`);
    h.check('fullscreen: the disconnect question is visible in full screen', await a.eval(`!!document.fullscreenElement && document.fullscreenElement.contains(document.querySelector('.dialog'))`));
    await a.clickText('.dialog button', t('common.cancel'));
    await a.eval(`document.exitFullscreen()`);
    await a.until(`!document.fullscreenElement && !s.info().full`, 'browser exit leaves full mode');
    h.check('fullscreen: leaving by the browser (Esc) restores the page', true);

    await a.eval(`document.title = 'e2e'`);
    const s1 = await a.v(`s.stats()`);
    await sleep(10000);
    const s2 = await a.v(`s.stats()`);
    const kbps = Math.round(((s2.bytes - s1.bytes) * 8) / 10 / 1000);
    console.log(`      stats: ${s2.w}x${s2.h} route=${s2.route} decoder=${s2.decoder} decodeMs=${s2.decodeMs?.toFixed(2)} lost=${s2.lost} dropped=${s2.dropped} frames+${s2.decoded - s1.decoded} ${kbps} kbps over 10 s`);
    h.check('video is decoded', s2.decoded > 0, `${s2.decoded} frames`);

    // `old` is decided from the computer's control hello only: a pointer position before it
    // (the move channel can be first) says nothing about the version.
    await a.v(`s.forgetHello()`);
    await a.v(`s.fromHost({ t: 'cp', x: 0.5, y: 0.5, v: true, s: 0 })`);
    h.check('a pointer position before the hello ends nothing', await a.v(`!!s && s.info().state === 'active'`));
    await a.v(`s.fromHost({ t: 'hello', version: ${MIN_VERSION - 1}, caps: [] })`);
    await a.until(`!!document.querySelector('.dialog')`, 'update notice');
    h.check('a hello older than this page ends the session: the update notice', await a.eval(`document.querySelector('.dialog h2')?.textContent === ${q('update.computerTitle')}`) && await a.v(`!s`));
    // The notice closed: the areas after this one use the page.
    await a.eval(`document.querySelector('.dialog button')?.click()`);
    await a.until(`!document.querySelector('.dialog')`, 'update notice closed');
  },
};
