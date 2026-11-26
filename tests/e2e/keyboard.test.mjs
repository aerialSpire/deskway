// A physical keyboard on the first device's session: real key events from DevTools on the
// screen, with what would go to the computer recorded. Keys go as they are (never remapped),
// paste keys included; the system-key hint; keys let go when the window is left; this device's
// input method and dictation, typed there as text.
// (The on-screen keyboard is checked with fingers, in touch.test.mjs.)
import { sleep, keyboardOf, q } from './harness.mjs';

export const area = {
  name: 'keyboard',
  needs: ['auth'],
  async run(h) {
    const a = h.a;
    await h.sessionOn(a);
    const { press, held } = keyboardOf(a);
    await a.v(`s.record()`);
    // Turning sharing on announces this device's clipboard (cb): not part of what keys send.
    const recorded = () => a.v(`JSON.stringify(s.recorded().filter((e) => !['sync', 'm', 'mr', 'cb'].includes(e.t)).map((e) => e.t === 'k' ? e.c + (e.d ? '+' : '-') : e.t))`);
    // Each check starts from nothing held, recording afresh, sharing on (paste keys included).
    const fresh = (os) => a.v(`(s.setRemoteOs('${os}'), s.fromHost({ t: 'clipshare', on: true }), s.releaseAll(), s.clearRecorded(), s.focus())`);

    await fresh('macos');
    await press('ControlLeft', true); await press('KeyC', true); await press('KeyC', false); await press('ControlLeft', false);
    h.check('to a Mac, Ctrl+C goes as Control+C: keys are sent as they are, never remapped', await recorded() === JSON.stringify(['ControlLeft+', 'KeyC+', 'KeyC-', 'ControlLeft-']), await recorded());
    await fresh('macos');
    await press('MetaLeft', true); await press('KeyC', true); await press('KeyC', false); await press('MetaLeft', false);
    h.check('to a Mac, the Win/Cmd key with C goes as Cmd+C', await recorded() === JSON.stringify(['MetaLeft+', 'KeyC+', 'KeyC-', 'MetaLeft-']), await recorded());
    h.check('outside full screen, the first Win or Cmd key says why such keys may not reach the computer', await a.eval(`!!document.querySelector('.keys-hint') && document.querySelector('.keys-hint').textContent.includes(${q('tb.full')})`));
    await a.eval(`document.querySelector('.keys-hint .x-btn').click()`);
    await press('MetaLeft', true); await press('MetaLeft', false);
    h.check('…once per session', await a.eval(`!document.querySelector('.keys-hint')`));

    // Paste keys are ordinary keys: nothing here reads the clipboard for them, nothing waits.
    // (Sharing on reads this device's clipboard to announce it, outside any key event: not counted.)
    await a.eval(`(() => { window.__reads = 0; const c = navigator.clipboard; c.read = async () => { if (window.event instanceof KeyboardEvent) window.__reads++; return []; }; c.readText = async () => { if (window.event instanceof KeyboardEvent) window.__reads++; return ''; }; })()`);
    await fresh('windows');
    await press('ControlLeft', true); await press('KeyV', true);
    const atOnce = await recorded();
    await press('KeyV', false); await press('ControlLeft', false);
    await sleep(300);
    h.check('Ctrl+V goes to the computer as Ctrl+V at once, with sharing on: paste keys are ordinary keys', atOnce === JSON.stringify(['ControlLeft+', 'KeyV+']) && await recorded() === JSON.stringify(['ControlLeft+', 'KeyV+', 'KeyV-', 'ControlLeft-']) && await a.eval(`window.__reads === 0`), await recorded());
    for (const [os, modifiers, code, key, vk] of [
      ['macos', ['MetaLeft'], 'KeyV', 'v', 86],
      ['windows', ['ControlLeft', 'ShiftLeft'], 'KeyV', 'V', 86],
      ['windows', ['ShiftLeft'], 'Insert', 'Insert', 45],
    ]) {
      await fresh(os);
      for (const mod of modifiers) await press(mod, true);
      await press(code, true, { key, windowsVirtualKeyCode: vk });
      const down = [...modifiers, code].map((c) => c + '+');
      const atOnce = await recorded();
      await press(code, false, { key, windowsVirtualKeyCode: vk });
      const release = modifiers.toReversed();
      for (const mod of release) await press(mod, false);
      const up = [code, ...release].map((c) => c + '-');
      h.check([...modifiers, code].join('+') + ' sends literal desktop keys without reading the clipboard',
        atOnce === JSON.stringify(down) && await recorded() === JSON.stringify([...down, ...up]) && await a.eval('window.__reads === 0'), await recorded());
    }
    await fresh('windows');
    await press('ControlLeft', true); await press('KeyC', true);
    await a.v(`document.activeElement.blur()`);
    held.clear();
    await sleep(300);
    h.check('leaving the window lets go of the keys held', await recorded() === JSON.stringify(['ControlLeft+', 'KeyC+', 'ControlLeft-', 'KeyC-']), await recorded());
    await a.v(`(s.setRemoteOs('windows'), s.focus())`);
    h.check('every key is let go after all that', await a.v(`s.info().pressed.length === 0`));
    h.check('closing the tab during a session asks first (Ctrl+W outside full screen)', await a.v(`s.leavePageAsks()`));

    // A physical keyboard has been used here: the screen's focus is its text box, which asks
    // for no system keyboard. What this device's input method makes goes as text.
    const texts = () => a.v(`JSON.stringify(s.recorded().filter((e) => e.t === 'x').map((e) => e.v))`);
    await fresh('windows');
    h.check('after a physical key, the screen keeps the focus in its text box, with no system keyboard', await a.v(`document.activeElement === s.el.box && s.el.box.getAttribute('inputmode') === 'none'`));
    await a.send('Input.dispatchKeyEvent', { type: 'rawKeyDown', code: 'KeyN', key: 'Process', windowsVirtualKeyCode: 229 });
    await a.send('Input.imeSetComposition', { text: 'ni', selectionStart: 2, selectionEnd: 2 });
    await a.send('Input.dispatchKeyEvent', { type: 'keyUp', code: 'KeyN', key: 'n', windowsVirtualKeyCode: 78 });
    h.check('a key the input method here takes is not sent, and the word being composed shows on the screen', await recorded() === '[]' && await a.v(`s.el.box.classList.contains('composing')`), await recorded());
    await a.send('Input.insertText', { text: '你' });
    await sleep(100);
    h.check('the chosen word is typed there as text, and the box hides again', await texts() === JSON.stringify(['你']) && await a.v(`!s.el.box.classList.contains('composing')`), await texts());
    await fresh('windows');
    await a.send('Input.insertText', { text: 'hello world' });
    await sleep(100);
    h.check('dictation (text put in at once) is typed there as text', await texts() === JSON.stringify(['hello world']), await texts());
    await fresh('windows');
    await press('ControlLeft', true); await press('KeyC', true); await press('KeyC', false); await press('ControlLeft', false);
    h.check('with the text box, keys still go as positions', await recorded() === JSON.stringify(['ControlLeft+', 'KeyC+', 'KeyC-', 'ControlLeft-']) && await texts() === '[]', await recorded());
    await a.v(`s.stopRecording()`);
  },
};
