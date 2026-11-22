// A phone (touch emulation, a phone's user agent and size): the page opening, the floating
// keyboard by itself, the mouse panel, the on-screen keyboard pressed with real fingers, the
// "Input method", gestures, the clipboard ball, the lock note, a physical keyboard, full screen by
// default, and every control on every page tapped with a real finger.
import { sleep, SCREENS, TAKE_OVER, centre, touch, tap, swipe, t, q, fixed } from './harness.mjs';

export const area = {
  name: 'touch',
  needs: ['auth'],
  async run(h) {
    await h.freeComputer(h.a);
    const m = await h.newPage();
    await m.addAuthenticator();
    await h.a.copyPasskeysTo(m);
    await m.send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 });
    await m.metrics(390, 844, true);
    await m.send('Emulation.setEmitTouchEventsForMouse', { enabled: true, configuration: 'mobile' });
    // Records every state the page passes through while it opens, to catch a false "no computers".
    await m.send('Page.addScriptToEvaluateOnNewDocument', { source: `window.__seen = []; new MutationObserver(() => { const t = document.body ? document.body.textContent : ''; for (const k of ${JSON.stringify([t('list.emptyTitle'), t('common.loading'), t('unreach.title')])}) if (t.includes(k) && !window.__seen.includes(k)) window.__seen.push(k); }).observe(document, { subtree: true, childList: true, characterData: true });` });
    await m.send('Emulation.setUserAgentOverride', { userAgent: 'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36' });
    await m.eval(`localStorage.removeItem('deskway.hwkbd')`);
    await m.eval(`location.reload()`);
    await sleep(800);
    await m.until(`!!document.querySelector('.card') && matchMedia('(pointer: coarse)').matches`, 'phone page with coarse pointer');
    const seen = await m.eval(`window.__seen`);
    h.check('opening the page shows loading, never a false "no computers" or "cannot connect"', seen.includes(t('common.loading')) && !seen.includes(t('list.emptyTitle')) && !seen.includes(t('unreach.title')), seen.join(' > '));
    await m.eval(`document.querySelector('.card-open').click()`);
    await m.until(SCREENS, 'phone detail page');
    await m.eval(TAKE_OVER);
    await m.until(`!!s && s.info().connection === 'connected' && s.info().w > 0`, 'phone video');
    await m.until(`s.info().input === 'open'`, 'phone input channel');
    h.check('a touch screen with no mouse: no "Lock pointer" (Pointer Lock needs a mouse)', await m.v(`s.el.lockBtn.hidden`), await m.v(`JSON.stringify({ fine: matchMedia('(any-pointer: fine)').matches, hidden: s.el.lockBtn.hidden })`));
    h.check('phone without a physical keyboard: the floating keyboard shows by itself, without popping up the phone keyboard', await m.v(`!!document.querySelector('.keypad') && !!s.el.modeBtn && document.activeElement !== s.el.typer`));
    h.check('phone upright: the whole picture sits above the keyboard, none of it underneath', await m.v(`(() => {
      const k = document.querySelector('.keypad').getBoundingClientRect(), v = s.videoRect(), st = s.el.stage.getBoundingClientRect();
      return st.bottom <= k.top + 0.5 && v.top + v.h <= k.top + 0.5 && v.h > 100;
    })()`), await m.v(`(() => { const k = document.querySelector('.keypad').getBoundingClientRect(), v = s.videoRect(); return 'video ' + Math.round(v.top) + '-' + Math.round(v.top + v.h) + ' keyboard ' + Math.round(k.top); })()`));
    h.check('closing the keyboard gives the room back to the picture', await m.v(`(() => {
      const withKb = s.el.stage.getBoundingClientRect().height;
      s.keyboard(false);
      const without = s.el.stage.getBoundingClientRect().height;
      s.keyboard(true, { auto: true });
      return without > withKb + 100 && Math.abs(s.el.stage.getBoundingClientRect().height - withKb) < 1;
    })()`));
    // From here on input is recorded in the page instead of being sent.
    await m.v(`s.record()`);
    const sent = () => m.v(`JSON.stringify(s.recorded().filter((e) => !['sync', 'm', 'mr'].includes(e.t)).map((e) => e.t === 'k' ? e.c + (e.d ? '+' : '-') : e.t === 'b' ? 'b' + e.b + (e.d ? '+' : '-') : e.t === 'x' ? 'x:' + e.v : e.t))`);
    const clearSent = () => m.v(`s.clearRecorded()`);

    // ---- the mouse panel: left, right and a scroll strip at the edge of the screen ----
    await m.v(`s.el.padBtn.click()`);
    await m.until(`!!document.querySelector('.mousepad')`, 'mouse panel');
    h.check('mouse panel: in portrait it is a row low on the screen, not over the picture', await m.v(`(() => {
      const p = document.querySelector('.mousepad').getBoundingClientRect(), v = s.videoRect();
      return document.querySelector('.mousepad').classList.contains('row') && p.top >= v.top + v.h - 1;
    })()`), await m.v(`(() => { const p = document.querySelector('.mousepad').getBoundingClientRect(), v = s.videoRect(), k = document.querySelector('.keypad')?.getBoundingClientRect(); return 'pad ' + Math.round(p.top) + '-' + Math.round(p.bottom) + ' video ' + Math.round(v.top) + '-' + Math.round(v.top + v.h) + ' keyboard ' + (k ? Math.round(k.top) + '-' + Math.round(k.bottom) : 'none') + ' window ' + innerHeight; })()`));
    // Dragged to another place, it stays there.
    await swipe(m, await centre(m, '.pad-grip'), [0, -30], 5);
    await sleep(150);
    h.check('mouse panel: the grip moves it, and the place is remembered', await m.eval(`!!localStorage.getItem('deskway.pad.portrait')`));
    await m.v(`(localStorage.removeItem('deskway.pad.portrait'), s.layout())`);
    await m.shot('phone-mousepad', 390, 844);
    await m.metrics(390, 844, true);
    const padTap = async (label, hold = 60) => {
      const r = await m.eval(`(() => { const b = [...document.querySelectorAll('.pad-btn')].find((x) => x.textContent === ${JSON.stringify(label)}).getBoundingClientRect(); return [b.left + b.width / 2, b.top + b.height / 2]; })()`);
      await tap(m, r, { hold });
      await sleep(150);
    };
    await clearSent();
    await padTap(t('pad.left'));
    h.check('mouse panel: Left clicks left', (await sent()).includes('"b0+","b0-"'), await sent());
    await clearSent();
    await padTap(t('pad.right'));
    h.check('mouse panel: Right clicks right', (await sent()).includes('"b2+","b2-"'), await sent());
    await clearSent();
    // Holding Left keeps the button down (to drag a window's edge with a finger on the screen); the
    // page is an app: the long press selects nothing and brings no menu of the browser's.
    await padTap(t('pad.left'), 800);
    const menuWithheld = await m.eval(`(() => { const e = new MouseEvent('contextmenu', { bubbles: true, cancelable: true }); document.querySelector('.pad-btn').dispatchEvent(e); return e.defaultPrevented; })()`);
    h.check('mouse panel: holding Left keeps the left button down, selecting nothing, with no browser menu', (await sent()) === JSON.stringify(['b0+']) && await m.eval(`getSelection().toString() === '' && document.querySelector('.pad-btn').classList.contains('down')`) && menuWithheld, await sent());
    await padTap(t('pad.left'));
    h.check('mouse panel: the next tap on Left lets the button go', (await sent()) === JSON.stringify(['b0+', 'b0-']), await sent());
    await clearSent();
    const strip = await m.eval(`(() => { const b = document.querySelector('.pad-scroll').getBoundingClientRect(); return [b.left + b.width / 2, b.top + 20]; })()`);
    await swipe(m, strip, [0, 15], 5);
    await sleep(150);
    const wheels = await m.v(`s.recorded().filter((e) => e.t === 'w').reduce((n, e) => n + e.dy, 0)`);
    h.check('mouse panel: dragging the strip down scrolls down, and nothing else is sent', wheels > 0 && !(await sent()).includes('b0'), `dy ${wheels}`);
    await m.v(`s.el.padBtn.click()`);
    h.check('mouse panel closes from the toolbar', await m.eval(`!document.querySelector('.mousepad')`));
    await clearSent();

    await onscreenKeyboard(h, m, sent, clearSent);
    await gesturesAndMore(h, m, sent, clearSent);
    await phoneTaps(h, m);
  },
};

// ---- the on-screen keyboard: a whole computer keyboard, pressed with real fingers ----
async function onscreenKeyboard(h, m, sent, clearSent) {
  const keyAt = (code) => centre(m, `.kb-key[data-code="${code}"]`);
  const tapKey = async (code) => {
    await tap(m, await keyAt(code), { hold: 40 });
    await sleep(60);
  };
  h.check('the bar above the keys: grip, then "Input method", and "Hide keyboard" at the right as an icon; the first row is all keys', await m.eval(`(() => {
    const bar = document.querySelector('.kb-bar'), grip = bar.querySelector('.kb-grip'), ime = bar.querySelector('.kb-ime'), hide = bar.querySelector('.kb-hide');
    const g = grip.getBoundingClientRect(), i = ime.getBoundingClientRect(), x = hide.getBoundingClientRect(), b = bar.getBoundingClientRect();
    const first = document.querySelector('.kb-row');
    return ime.textContent === ${q('kb.ime')} && i.left > g.right - 1 && i.left - g.right < 16 && x.right > b.right - 8 && !!hide.querySelector('svg') && hide.textContent.trim() === ''
      && first.firstElementChild.dataset.code === 'Escape' && first.lastElementChild.dataset.code === 'Delete';
  })()`));
  // Under the mouse, "Input method" explains itself at once; a finger never shows it.
  const imeAt = await centre(m, '.kb-ime');
  await m.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: imeAt[0], y: imeAt[1] });
  await sleep(100);
  h.check('the mouse over "Input method" shows "Opens the system keyboard on phones" at once, inside the window', await m.eval(`(() => { const t = document.querySelector('.kb-tip'); if (t.hidden) return false; const r = t.getBoundingClientRect(); return t.textContent === ${q('kb.imeTip')} && r.left >= 0 && r.right <= innerWidth && r.top >= 0; })()`));
  await m.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: 5, y: 5 });
  await sleep(100);
  h.check('…and hides when the mouse leaves', await m.eval(`document.querySelector('.kb-tip').hidden`));
  h.check('the keyboard is a whole computer keyboard across the bottom: F1–F12, letters, digits, Ctrl/Win/Alt/Shift both sides, arrows, "Input method" and "Hide keyboard"', await m.eval(`(() => {
    const has = (c) => !!document.querySelector('.kb-key[data-code="' + c + '"]');
    const codes = ['Escape', 'F1', 'F12', 'Delete', 'Backquote', 'Digit1', 'Digit0', 'Minus', 'Equal', 'Backspace', 'Tab', 'KeyQ', 'KeyP', 'BracketLeft', 'Backslash', 'CapsLock', 'KeyA', 'Semicolon', 'Quote', 'Enter', 'ShiftLeft', 'KeyZ', 'Slash', 'ShiftRight', 'ArrowUp', 'ControlLeft', 'MetaLeft', 'AltLeft', 'Space', 'AltRight', 'ControlRight', 'ArrowLeft', 'ArrowDown', 'ArrowRight'];
    const k = document.querySelector('.keypad').getBoundingClientRect();
    return codes.every(has) && !!document.querySelector('.kb-ime') && !!document.querySelector('.kb-hide') && !!document.querySelector('.kb-grip')
      && Math.abs(k.left) < 2 && Math.abs(k.width - innerWidth) < 2 && Math.abs(k.bottom - innerHeight) < 2
      && !document.querySelector('.combos') && ![...document.querySelectorAll('.keypad button')].some((b) => b.textContent === '⋯');
  })()`));
  const keyProblems = (minH) => m.eval(`[...document.querySelectorAll('.keypad .kb-key')].map((b) => {
    const r = b.getBoundingClientRect();
    const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
    const ok = r.width >= 20 && r.height >= ${minH} && r.left >= 0 && r.right <= innerWidth + 0.5 && r.bottom <= innerHeight + 0.5 && (hit === b || b.contains(hit));
    return ok ? null : (b.dataset.code || b.textContent) + ' ' + Math.round(r.left) + ',' + Math.round(r.top) + ' ' + r.width.toFixed(1) + 'x' + r.height.toFixed(1) + ' hit ' + (hit ? hit.className || hit.tagName : 'none');
  }).filter(Boolean).join('; ')`);
  const upright = await keyProblems(24);
  h.check('every key on a phone held upright is on screen, not covered, and wide enough to hit', upright === '', upright);
  await clearSent();

  // Three fingers: Ctrl and Shift held, T tapped. The remote computer gets Ctrl+Shift+T.
  const ctrl = await keyAt('ControlLeft'), shift = await keyAt('ShiftLeft'), tKey = await keyAt('KeyT');
  await touch(m, 'touchStart', [ctrl]);
  await sleep(60);
  await touch(m, 'touchStart', [ctrl, shift]);
  await sleep(60);
  h.check('two modifiers held: both light up, nothing sent yet', await m.eval(`[...document.querySelectorAll('.kb-key[data-code="ControlLeft"], .kb-key[data-code="ShiftLeft"]')].every((b) => b.getAttribute('aria-pressed') === 'true')`) && await sent() === '[]', await sent());
  await touch(m, 'touchStart', [ctrl, shift, tKey]);
  await sleep(60);
  h.check('several fingers at once: Ctrl and Shift held, T pressed: T goes down with Ctrl and Shift already down', await sent() === JSON.stringify(['ControlLeft+', 'ShiftLeft+', 'KeyT+']), await sent());
  await touch(m, 'touchEnd', []);
  await sleep(80);
  const lifted = JSON.parse(await sent()).slice(3);
  h.check('…and lifting the fingers lets go of all three', lifted.length === 3 && ['ControlLeft-', 'ShiftLeft-', 'KeyT-'].every((x) => lifted.includes(x)), JSON.stringify(lifted));
  h.check('held modifiers are not left on afterwards', await m.v(`Object.keys(s.info().mods).length === 0 && !document.querySelector('.kb-key.mod[aria-pressed="true"]')`));
  await clearSent();

  // The screen keyboard applies Ctrl and Cmd to one key, then releases both before F.
  for (const code of ['ControlLeft', 'MetaLeft', 'KeyQ', 'KeyF']) await tapKey(code);
  h.check('tapping Ctrl, Cmd, Q and F applies the chord to Q only', await sent() === JSON.stringify(['ControlLeft+', 'MetaLeft+', 'KeyQ+', 'KeyQ-', 'MetaLeft-', 'ControlLeft-', 'KeyF+', 'KeyF-']), await sent());
  await clearSent();

  // Alt held while Tab is tapped twice: the window switcher stays open until Alt lifts.
  await touch(m, 'touchStart', [await keyAt('AltLeft')]);
  await sleep(50);
  // The second finger taps Tab twice. DevTools cannot lift one finger while another stays, so
  // these two taps are pointer events made in the page, the same events a finger produces.
  for (let i = 0; i < 2; i++) {
    await m.eval(`(() => { const t = document.querySelector('.kb-key[data-code="Tab"]'); const o = { pointerId: 77, pointerType: 'touch', isPrimary: false, bubbles: true, cancelable: true, button: 0 }; t.dispatchEvent(new PointerEvent('pointerdown', o)); t.dispatchEvent(new PointerEvent('pointerup', o)); })()`);
    await sleep(40);
  }
  h.check('Alt held, Tab tapped twice: Alt stays down on the remote computer the whole time', await sent() === JSON.stringify(['AltLeft+', 'Tab+', 'Tab-', 'Tab+', 'Tab-']), await sent());
  await touch(m, 'touchEnd', []);
  await sleep(60);
  h.check('…and goes up when the finger lifts', (await sent()).endsWith('"AltLeft-"]'), await sent());
  await clearSent();

  // One hand: tap Ctrl, then C.
  const mods = () => m.v(`JSON.stringify(s.info().mods)`);
  await tapKey('ControlLeft');
  h.check('one hand: a tapped modifier lights up and sends nothing by itself', await mods() === '{"ControlLeft":1}' && await m.eval(`document.querySelector('.kb-key[data-code="ControlLeft"]').getAttribute('aria-pressed') === 'true'`) && await sent() === '[]', await sent());
  await tapKey('KeyC');
  h.check('one hand: Ctrl tapped, then C, sends Ctrl+C; Ctrl then goes off', await sent() === JSON.stringify(['ControlLeft+', 'KeyC+', 'KeyC-', 'ControlLeft-']) && await mods() === '{}', await sent());
  await clearSent();
  await tapKey('MetaLeft');
  await sleep(450);
  await tapKey('MetaLeft');
  h.check('a lone Win tapped on and off sends nothing (no Start menu)', await sent() === '[]' && await mods() === '{}', await sent());
  // Twice quickly: stays on for several keys, until tapped again.
  await tapKey('ControlLeft');
  await tapKey('ControlLeft');
  h.check('a modifier tapped twice quickly stays on', await mods() === '{"ControlLeft":2}' && await m.eval(`document.querySelector('.kb-key[data-code="ControlLeft"]').classList.contains('locked')`));
  await tapKey('KeyA');
  await tapKey('KeyV');
  await tapKey('ControlLeft');
  h.check('locked Ctrl: A then V both go with Ctrl; one more tap turns it off', await sent() === JSON.stringify(['ControlLeft+', 'KeyA+', 'KeyA-', 'ControlLeft-', 'ControlLeft+', 'KeyV+', 'KeyV-', 'ControlLeft-']) && await mods() === '{}', await sent());
  await clearSent();
  await tapKey('ShiftLeft');
  h.check('with Shift on, the keys show what they type with Shift', await m.eval(`document.querySelector('.keypad').classList.contains('shift') && getComputedStyle(document.querySelector('.kb-key[data-code="Digit1"] .kb-alt')).display !== 'none'`));
  await tapKey('Digit1');
  h.check('Shift tapped, then 1, sends Shift+1 (!)', await sent() === JSON.stringify(['ShiftLeft+', 'Digit1+', 'Digit1-', 'ShiftLeft-']) && await m.eval(`!document.querySelector('.keypad').classList.contains('shift')`), await sent());
  await clearSent();

  // A key held repeats, as on a real keyboard.
  await tap(m, await keyAt('Backspace'), { hold: 900 });
  await sleep(80);
  const reps = JSON.parse(await sent());
  h.check('holding ⌫ repeats it, then lets go once', reps.filter((x) => x === 'Backspace+').length >= 4 && reps.at(-1) === 'Backspace-' && reps.filter((x) => x === 'Backspace-').length === 1, `${reps.length} events`);
  await clearSent();
  // Shift held a moment alone goes down, so Shift+click works; closing the keyboard lets go.
  await touch(m, 'touchStart', [shift]);
  await sleep(550);
  h.check('a modifier held alone a moment goes down on the remote computer', await sent() === JSON.stringify(['ShiftLeft+']), await sent());
  await m.v(`s.keyboard(false)`);
  await touch(m, 'touchEnd', []);
  h.check('closing the keyboard lets go of keys still held', await sent() === JSON.stringify(['ShiftLeft+', 'ShiftLeft-']), await sent());
  await m.v(`s.keyboard(true, { auto: true })`);
  await clearSent();

  // "Input method": the system's input method, for words.
  await tapKey('Escape');
  await clearSent();
  await tap(m, await centre(m, '.kb-ime'));
  await sleep(150);
  h.check('"Input method" opens the system keyboard, and sends no key, nor its explanation', await m.v(`document.activeElement === s.el.typer && document.querySelector('.kb-ime').getAttribute('aria-pressed') === 'true' && document.querySelector('.kb-tip').hidden`) && await sent() === '[]', await sent());
  await m.send('Input.imeSetComposition', { text: 'ni', selectionStart: 2, selectionEnd: 2 });
  await sleep(100);
  h.check('"Input method": the word being typed shows on the keyboard and is not sent yet', await m.eval(`!document.querySelector('.kb-preview').hidden && document.querySelector('.kb-preview').textContent === 'ni'`) && await sent() === '[]', await sent());
  await m.send('Input.insertText', { text: '你' });
  await sleep(100);
  h.check('"Input method": the chosen word goes to the remote cursor', await sent() === JSON.stringify(['x:你']), await sent());
  await clearSent();
  await m.v(`(() => { const t = s.el.typer; t.value = '\\u200b\\u200b你好'; t.dispatchEvent(new InputEvent('input', { inputType: 'insertText', data: '你好' })); t.dispatchEvent(new InputEvent('input', { inputType: 'deleteContentBackward' })); t.dispatchEvent(new InputEvent('input', { inputType: 'insertLineBreak' })); })()`);
  h.check('"Input method": typing sends text, Backspace and Enter', await sent() === JSON.stringify(['x:你好', 'Backspace+', 'Backspace-', 'Enter+', 'Enter-']), await sent());
  await clearSent();
  // The controller waits for the viewport to give up room before collapsing its keys.
  const keysShown = `!!document.querySelector('.kb-row').offsetParent`;
  h.check('"Input method" requested: without a viewport change the keys are still visible', await m.v(`${keysShown} && !s.el.stage.classList.contains('ime-stage')`));
  // The phone keyboard folded away by the phone's own button: the text box keeps the focus, only
  // the visible area grows back. (A stand-in visual viewport: this browser shows no keyboard.)
  await m.v(`(() => {
    const real = window.visualViewport, page = document.documentElement.clientHeight;
    window.__vv = { width: real.width, height: page - 300, scale: 1, offsetLeft: 0, offsetTop: 0 };
    Object.defineProperty(window, 'visualViewport', { configurable: true, get: () => window.__vv });
    window.__vvReal = real;
    real.dispatchEvent(new Event('resize'));
  })()`);
  h.check('"Input method": while the phone keyboard shows, the keys stay aside', await m.v(`!(${keysShown}) && document.querySelector('.kb-ime').getAttribute('aria-pressed') === 'true'`));
  h.check('the picture stage ends at the keyboard bar, and the bar ends at the visible viewport', await m.v(`(() => {
    const bar = document.querySelector('.keypad').getBoundingClientRect();
    const stage = s.el.stage.getBoundingClientRect();
    return Math.abs(stage.bottom - bar.top) < 2 && Math.abs(bar.bottom - visualViewport.height) < 2;
  })()`));
  await m.v(`(() => { window.__vv.height = document.documentElement.clientHeight; window.__vvReal.dispatchEvent(new Event('resize')); })()`);
  h.check('the phone keyboard folded away: the keys come back and "Input method" is released', await m.v(`${keysShown} && document.querySelector('.kb-ime').getAttribute('aria-pressed') === 'false' && document.activeElement !== s.el.typer`));
  await m.v(`(() => { delete window.visualViewport; window.__vvReal.dispatchEvent(new Event('resize')); })()`);
  await tap(m, await centre(m, '.kb-ime'));
  await sleep(150);
  await tap(m, await centre(m, '.kb-ime'));
  await sleep(150);
  h.check('"Input method" again puts the phone keyboard away and brings the keys back', await m.v(`document.activeElement !== s.el.typer && document.querySelector('.kb-ime').getAttribute('aria-pressed') === 'false' && ${keysShown}`));
  await clearSent();

  // "Hide keyboard", and back from the toolbar.
  await tap(m, await centre(m, '.kb-hide'));
  await sleep(150);
  h.check('"Hide keyboard" puts the keyboard away', await m.v(`!document.querySelector('.keypad') && s.el.kbdBtn.getAttribute('aria-pressed') === 'false'`) && await sent() === '[]', await sent());
  await m.v(`s.el.kbdBtn.click()`);
  h.check('"Keyboard" in the toolbar brings it back', await m.eval(`!!document.querySelector('.keypad')`));

  // Moved up by its grip; remembered; the screen does not move and nothing is sent.
  const stageBefore = await m.eval(`JSON.stringify(document.querySelector('.stage').getBoundingClientRect())`);
  await swipe(m, await centre(m, '.kb-grip'), [0, -40], 6);
  await sleep(150);
  h.check('the keyboard is moved up by its bar, remembered, and the screen stays put', await m.eval(`!!localStorage.getItem('deskway.kb.portrait') && document.querySelector('.keypad').getBoundingClientRect().bottom < innerHeight - 150`) && stageBefore === await m.eval(`JSON.stringify(document.querySelector('.stage').getBoundingClientRect())`) && await sent() === '[]', await sent());
  await m.v(`(localStorage.removeItem('deskway.kb.portrait'), s.layout())`);
  await m.shot('phone-keyboard', 390, 844);
  await m.shot('phone-keyboard-landscape', 844, 390);
  await m.metrics(844, 390, true);
  await sleep(200);
  await m.v(`s.layout()`);
  const sideways = await keyProblems(20);
  h.check('held sideways, every key is on screen and not covered', sideways === '', sideways);
  await m.metrics(390, 844, true);
  await sleep(200);
  await m.v(`s.layout()`);
  await m.v(`(s.keyboard(false), s.keyboard(true, { auto: true }))`);
  await clearSent();
}

// Gestures, the clipboard ball, the lock note, a physical keyboard, full screen by default.
async function gesturesAndMore(h, m, sent, clearSent) {
  // Real touch events at the centre of the screen. The test host reports where its real mouse
  // is, which would move the pointer under the test, so those reports are ignored.
  await m.v(`(s.ignorePointerReports(), s.setCursor(0.5, 0.5))`);
  const box = await m.v(`(() => { const r = s.el.stage.getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })()`);
  const cursorX = () => m.v(`s.info().pointer.x`);
  await clearSent();
  await tap(m, [box.x, box.y]);
  await sleep(100);
  h.check('phone trackpad: a tap clicks', (await sent()).includes('"b0+","b0-"'), await sent());
  await clearSent();
  const c0 = await cursorX();
  await swipe(m, [box.x, box.y], [4, 0], 10);
  const slow = await cursorX() - c0;
  const c1 = await cursorX();
  await swipe(m, [box.x - 60, box.y], [20, 0], 2);
  const fast = await cursorX() - c1;
  h.check('phone trackpad: fast strokes move further than slow ones over the same distance', fast > slow * 1.3 && !(await sent()).includes('b0'), `slow ${slow.toFixed(3)} fast ${fast.toFixed(3)}`);
  h.check('phone trackpad: the pointer is drawn on the page', await m.v(`!s.el.cursor.hidden && s.el.cursor.getBoundingClientRect().width > 8`));
  await clearSent();
  const wheels = `s.recorded().filter((e) => e.t === 'w').length`;
  await touch(m, 'touchStart', [[box.x - 20, box.y], [box.x + 20, box.y]]);
  for (let i = 1; i <= 6; i++) { await touch(m, 'touchMove', [[box.x - 20, box.y - i * 12], [box.x + 20, box.y - i * 12]]); await sleep(16); }
  await touch(m, 'touchEnd', []);
  const during = await m.v(wheels);
  await sleep(400);
  const total = await m.v(wheels);
  h.check('phone: two fingers scroll, and keep scrolling briefly after lifting', during > 0 && total > during, `${during} while touching, ${total - during} after`);
  await clearSent();
  await m.v(`s.clearViews()`);
  const before = await m.v(`s.info().view`);
  await touch(m, 'touchStart', [[box.x - 20, box.y], [box.x + 20, box.y]]);
  for (let i = 1; i <= 8; i++) { await touch(m, 'touchMove', [[box.x - 20 - i * 15, box.y], [box.x + 20 + i * 15, box.y]]); await sleep(16); }
  await sleep(100);
  const duringPinch = await m.v(`s.viewsSent().length`);
  await touch(m, 'touchEnd', []);
  h.check('phone: pinch zooms in', await m.v(`s.info().zoom > 1.5 && s.info().zoom <= 6`), await m.v(`s.info().zoom.toFixed(2)`));
  await m.until(`s.viewsSent().length > 0`, 'view after the pinch', 3000).catch(() => {});
  const pinchViews = await m.v(`s.viewsSent()`);
  h.check('phone: a pinch tells the host the zoomed view once, when the fingers lift, not at every step', duringPinch === 0 && pinchViews.length === 1 && pinchViews[0].w > before.w, `${duringPinch} while pinching, then ${JSON.stringify(pinchViews)}`);
  // Zoomed in, upright, the keyboard closing and opening: what was in the middle of the stage
  // stays in the middle, measured on the element as drawn (not the page's own sums).
  const middle = `(() => {
    const st = s.el.stage.getBoundingClientRect(), el = s.el.video.getBoundingClientRect(), vid = s.el.video;
    const fit = Math.min(el.width / vid.videoWidth, el.height / vid.videoHeight);
    const left = el.left + (el.width - vid.videoWidth * fit) / 2, top = el.top + (el.height - vid.videoHeight * fit) / 2;
    return { u: (st.left + st.width / 2 - left) / (vid.videoWidth * fit), v: (st.top + st.height / 2 - top) / (vid.videoHeight * fit), h: Math.round(st.height) };
  })()`;
  await m.v(`s.keyboard(true, { auto: true })`);
  await sleep(300);
  const withKb = await m.v(middle);
  await m.v(`s.keyboard(false)`);
  await sleep(300);
  const closed = await m.v(middle);
  await m.v(`s.keyboard(true, { auto: true })`);
  await sleep(300);
  const reopened = await m.v(middle);
  const same = (a, b) => Math.abs(a.u - b.u) < 0.01 && Math.abs(a.v - b.v) < 0.01;
  h.check('phone upright, zoomed in: closing and opening the keyboard keeps the same part of the picture in the middle', closed.h > withKb.h + 100 && same(withKb, closed) && same(withKb, reopened), JSON.stringify({ withKb, closed, reopened, zoom: await m.v(`s.info().zoom`) }));
  await m.v(`s.clearViews()`);
  await m.v(`s.resetZoom()`);
  await m.until(`s.viewsSent().length > 0`, 'view after the zoom reset', 3000).catch(() => {});
  h.check('phone: zooming back out in one step tells the host once', await m.v(`s.viewsSent().length === 1 && s.viewsSent()[0].w === ${before.w}`), await m.v(`JSON.stringify(s.viewsSent())`));
  // A page being hidden can lay the stage out with no area for a moment: nothing is reported
  // then, and the same stage coming back changes nothing.
  await m.v(`s.clearViews()`);
  await m.v(`s.el.stage.style.display = 'none'`);
  await sleep(300);
  const noArea = await m.v(`s.viewsSent()`);
  await m.v(`s.el.stage.style.display = ''`);
  await sleep(300);
  const back = await m.v(`s.viewsSent()`);
  h.check('phone: a stage with no area tells the host nothing, and coming back the same changes nothing', noArea.length === 0 && back.length === 0, JSON.stringify({ noArea, back }));

  await m.v(`s.el.modeBtn.click()`);
  h.check('phone: the touch mode switches and is remembered', await m.v(`s.info().direct && localStorage.getItem('deskway.touch') === 'direct'`), await m.v(`s.el.modeBtn.textContent`));
  await clearSent();
  await tap(m, [box.x + 50, box.y + 30]);
  await sleep(100);
  const target = await m.v(`s.toNorm(${box.x + 50}, ${box.y + 30})`);
  const tapped = await m.v(`s.recorded().find((e) => e.t === 'b' && e.d)`);
  h.check('phone tap mode: a tap clicks where the finger is', !!tapped && Math.abs(tapped.x - target.x) < 0.002 && Math.abs(tapped.y - target.y) < 0.002, tapped ? `${tapped.x.toFixed(3)},${tapped.y.toFixed(3)}` : 'no click');
  await clearSent();
  await tap(m, [box.x, box.y], { hold: 550 });
  await sleep(100);
  h.check('phone tap mode: holding clicks right', (await sent()).includes('"b2+","b2-"'), await sent());
  await m.v(`s.el.modeBtn.click()`);

  await m.metrics(390, 844, true);
  await m.shot('phone-mousepad-portrait', 390, 844);
  await m.metrics(844, 390, true);
  await m.shot('phone-landscape', 844, 390);
  await m.v(`s.fromHost({ t: 'desk', v: 'winlogon' })`);
  h.check('the toolbar says when the remote computer is locked', await m.v(`!s.el.lockNote.hidden && s.el.lockNote.offsetWidth > 0`));
  await m.v(`s.fromHost({ t: 'desk', v: 'default' })`);
  h.check('the lock note goes away after unlocking', await m.v(`s.el.lockNote.hidden`));
  await m.v(`s.focus()`);
  await m.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'a', code: 'KeyA', windowsVirtualKeyCode: 65, text: 'a' });
  await m.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'a', code: 'KeyA', windowsVirtualKeyCode: 65 });
  await sleep(100);
  h.check('phone with a physical keyboard: keys go through and the floating keyboard steps aside', await m.eval(`!document.querySelector('.keypad') && localStorage.getItem('deskway.hwkbd') === '1'`) && (await sent()).includes('KeyA+'), await sent());
  await m.v(`s.leave()`);
  await m.metrics(1440, 900);

  // Taking over goes full screen, when the setting is on (the default).
  await m.eval(`localStorage.removeItem('deskway.autofull')`);
  await m.until(SCREENS, 'detail page before full screen test');
  // The computer has finished with the last session, or taking over would rightly ask to
  // disconnect it.
  await m.until(`d.devices()[0] && !d.devices()[0].controller`, 'previous session ended at the computer');
  await m.send('Runtime.evaluate', { expression: TAKE_OVER, userGesture: true });
  await m.until(`!!s && s.info().connection === 'connected'`, 'session with full screen');
  h.check('taking over goes full screen by default', await m.v(`!!document.fullscreenElement && s.info().full`));
  h.check('a device that has a physical keyboard does not show the floating keyboard', await m.eval(`!document.querySelector('.keypad')`));
  await m.v(`s.fullscreen()`);
  await m.until(`!document.fullscreenElement`, 'full screen off');
  h.check('full screen can be left by hand', await m.v(`!s.info().full && !!s.info().connection`));
  await m.v(`s.leave()`);
  await m.until(`!!document.querySelector('.heading .more')`, 'phone detail page after full screen');
  await m.metrics(390, 844, true);
  await sleep(300);
  const tapAt = await centre(m, '.heading .more');
  h.check('phone after a full-screen session: the ⋯ is not covered', await m.eval(`!!document.elementFromPoint(${tapAt[0]}, ${tapAt[1]})?.closest('.heading .more') && !document.fullscreenElement`), await m.eval(`(document.elementFromPoint(${tapAt[0]}, ${tapAt[1]})?.outerHTML || 'nothing').slice(0, 160) + ' full=' + !!document.fullscreenElement`));
  await tap(m, tapAt);
  await sleep(400);
  h.check('phone: a tap on the ⋯ opens the computer\'s menu', await m.eval(`!!document.querySelector('.heading .menu:not([hidden])')`));
  await m.eval(`document.querySelector('.heading .more').click()`);
  await m.metrics(1440, 900);
  await m.eval(`localStorage.setItem('deskway.autofull', '0')`);
}

// ---- phone: every control on every page, with a real finger ----
// Each tap is a real touch at the control's place, as on a phone: covered, moved or rebuilt
// controls fail here, where a script's .click() would still pass.
async function phoneTaps(h, p) {
  const center = (expr) => p.eval(`(() => { const e = ${expr}; if (!e) return null; e.scrollIntoView({ block: 'center' }); const r = e.getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; })()`);
  // `during`: something the page receives between finger down and finger up.
  const tapOn = async (what, expr, during) => {
    const at = await center(expr);
    if (!at) { h.check(`phone tap: ${what} is on the page`, false); return false; }
    await sleep(150);
    const hit = await p.eval(`(() => { const e = ${expr}, x = document.elementFromPoint(${at[0]}, ${at[1]}); return !!e && !!x && (e === x || e.contains(x)); })()`);
    if (!hit) h.check(`phone tap: ${what} is not covered`, false, await p.eval(`(document.elementFromPoint(${at[0]}, ${at[1]})?.outerHTML || 'nothing').slice(0, 160)`));
    await tap(p, at, { hold: 90, during });
    await sleep(350);
    return hit;
  };
  const btn = (scope, text) => `[...document.querySelectorAll(${JSON.stringify(scope)})].find((x) => x.textContent.trim() === ${JSON.stringify(text)})`;
  const dialogTitle = () => p.eval(`document.querySelector('.dialog h2')?.textContent || ''`);
  const cancelDialog = async (what) => {
    const ok = await tapOn(`${what}: Cancel`, btn('.dialog button', t('common.cancel')));
    h.check(`phone: ${what} closes with Cancel`, ok && await p.eval(`!document.querySelector('.dialog')`));
  };
  // The server tells every page about the computers again, as it does whenever one changes.
  const push = `d.serverSays({ type: 'devices', devices: d.devices() })`;

  await p.v(`d.show('list')`);
  await p.until(`!!document.querySelector('.card')`, 'phone list');
  await tapOn('account name', `document.querySelector('.topbar .name-btn')`);
  h.check('phone: tapping the account name opens its menu', await p.eval(`!document.querySelector('.topbar .name-btn + .menu').hidden`));
  await tapOn('menu My devices', btn('.menu-item', t('devices.title')));
  await p.until(`document.querySelectorAll('.device-row').length >= 2`, 'phone: my devices');
  h.check('phone: My devices opens from the menu', true);

  // A device's actions are under its ⋯: this browser ("This device") in the first card, the computer in the second.
  const rows = { 'this device': `[...document.querySelectorAll('.device-row')].find((r) => r.textContent.includes(${q('devices.tagThis')}))`, computer: `document.querySelector('.device-item .device-row')` };
  const more = (kind) => `${rows[kind]}?.querySelector('.device-actions .more')`;
  const item = (kind, text) => `[...(${rows[kind]})?.querySelectorAll('.menu-item') || []].find((b) => b.textContent.trim() === ${JSON.stringify(text)})`;
  for (const [kind, text, title] of [['this device', t('devices.signOut'), t('devices.signOutTitle')], ['computer', t('common.rename'), ''], ['computer', t('computer.removeFromAccount'), fixed('computer.removeTitle')]]) {
    for (const during of [null, push]) {
      const how = during ? ' while the server sends an update' : '';
      await tapOn(`${kind} ⋯`, more(kind));
      await tapOn(`${kind} ${text}${how}`, item(kind, text), during);
      const opened = await p.eval(`!!document.querySelector('.dialog')`);
      h.check(`phone My devices: "${text}" (${kind}) opens its dialog${how}`, opened && (await dialogTitle()).includes(title), await dialogTitle());
      if (opened) await cancelDialog(`${kind} ${text}`);
    }
  }
  h.check('phone My devices: cancelling removed nothing', await p.eval(`document.querySelectorAll('.device-row').length`) >= 2);

  const sw = (label) => `[...document.querySelectorAll('.setting-row')].find((r) => r.textContent.includes(${JSON.stringify(label)}))?.querySelector('.switch')`;
  for (const during of [null, push]) {
    const how = during ? ' while the server sends an update' : '';
    await tapOn(`Device checks${how}`, sw(t('devices.checks')), during);
    h.check(`phone My devices: the Device checks switch asks before turning off${how}`, (await dialogTitle()).includes(t('devices.offTitle')), await dialogTitle());
    if (await p.eval(`!!document.querySelector('.dialog')`)) await cancelDialog('Device checks');
    h.check(`phone My devices: Device checks stay on after Cancel${how}`, await p.eval(`${sw(t('devices.checks'))}.getAttribute('aria-checked') === 'true'`));
  }

  await p.eval(`localStorage.setItem('deskway.autofull', '1'); window.__seen = []`);
  await p.v(`d.show('list')`);
  await p.v(`d.show('devices')`);
  await p.until(`!!(${sw(t('devices.autofull'))})`, 'devices page again');
  await center(sw(t('devices.autofull')));
  await p.eval(`window.__seen = []`);
  const before = await p.eval(`({ y: scrollY, t: performance.timeOrigin })`);
  await tapOn('Full screen when taking control', sw(t('devices.autofull')));
  const after = await p.eval(`({ y: scrollY, t: performance.timeOrigin })`);
  h.check('phone My devices: Full screen when taking control switches off and is remembered', await p.eval(`${sw(t('devices.autofull'))}.getAttribute('aria-checked') === 'false' && localStorage.getItem('deskway.autofull') === '0'`));
  h.check('phone My devices: the switch does not reload the page, flash loading or jump', after.t === before.t && Math.abs(after.y - before.y) < 4 && !(await p.eval(`window.__seen.includes(${q('common.loading')})`)), `scroll ${before.y} -> ${after.y}, seen ${await p.eval(`window.__seen.join()`)}`);
  await tapOn('Full screen when taking control, again', sw(t('devices.autofull')), push);
  h.check('phone My devices: and back on, even while the server sends an update', await p.eval(`${sw(t('devices.autofull'))}.getAttribute('aria-checked') === 'true' && localStorage.getItem('deskway.autofull') === '1'`));
  await p.eval(`window.__seen = []`);
  const y0 = await p.eval(`scrollY`);
  await p.v(push);
  await sleep(400);
  h.check('phone My devices: an update from the server does not flash loading or move the page', !(await p.eval(`window.__seen.includes(${q('common.loading')})`)) && Math.abs((await p.eval(`scrollY`)) - y0) < 4);
  await p.eval(`localStorage.setItem('deskway.autofull', '0')`);

  await tapOn('back to My computers', `document.querySelector('.title-back')`);
  h.check('phone My devices: "My computers" goes back', await p.v(`d.view() === 'list'`));

  await tapOn('account name', `document.querySelector('.topbar .name-btn')`);
  await tapOn('menu Account', btn('.menu-item', t('account.title')));
  h.check('phone: Account opens from the menu', await p.v(`d.view() === 'account'`));
  const changeRow = (label) => `[...document.querySelectorAll('.setting-row')].find((r) => r.firstElementChild.firstElementChild.textContent === ${JSON.stringify(label)})?.querySelector('button')`;
  for (const row of [t('account.display'), t('account.name'), t('account.password'), t('account.super')]) {
    await tapOn(`Account: ${row}: Change`, changeRow(row));
    const opened = await p.eval(`!!document.querySelector('.dialog.form')`);
    h.check(`phone Account: "Change" on ${row} opens its dialog`, opened, await dialogTitle());
    if (opened) await cancelDialog(row);
  }
  await tapOn('back from Account', `document.querySelector('.title-back')`);
  h.check('phone Account: the mark before the title goes back', await p.v(`d.view() === 'list'`));

  for (const during of [null, push]) {
    const how = during ? ' while the server sends an update' : '';
    await p.v(`d.show('list')`);
    await p.until(`!!document.querySelector('.card')`, 'phone list');
    // Holding a card with a finger opens its menu; lifting does not open the computer.
    await tap(p, await centre(p, '.card .thumb'), { hold: 800, during });
    await sleep(350);
    h.check(`phone: holding a computer's card opens its menu, and lifting does not open it${how}`, await p.eval(`!!document.querySelector('.card .menu:not([hidden])')`) && await p.v(`d.view() === 'list'`), await p.v(`d.view()`));
    await tapOn(`the card's ⋯ closes its menu${how}`, `document.querySelector('.card .more')`);
    h.check(`phone: the card's ⋯ closes its open menu${how}`, await p.eval(`!document.querySelector('.card .menu:not([hidden])')`));
    await tapOn(`computer card${how}`, `document.querySelector('.card-open')`, during);
    h.check(`phone: tapping a computer opens it${how}`, await p.v(`d.view() === 'detail'`));
    await p.until(`!!document.querySelector('.heading .more')`, 'phone detail');
    await tapOn(`⋯${how}`, `document.querySelector('.heading .more')`, during);
    h.check(`phone: the ⋯ opens the computer's menu${how}`, await p.eval(`!!document.querySelector('.heading .menu:not([hidden])')`));
    await tapOn(`Rename${how}`, btn('.heading .menu-item', t('common.rename')));
    h.check(`phone: Rename from the ⋯ opens the rename form${how}`, await p.eval(`!!document.querySelector('.dialog.form input')`));
    if (await p.eval(`!!document.querySelector('.dialog')`)) await cancelDialog('Rename');
    await tapOn(`back${how}`, `document.querySelector('.title-back')`, during);
    h.check(`phone: back from a computer${how}`, await p.v(`d.view() === 'list'`));
  }
  await p.metrics(1440, 900);
}
