// Input as the browser sends it, on the first device's session (the test host is view only, so
// what reaches the computer is checked in the host's unit tests): lock and modifier sync, keys
// let go under Command on a Mac, characters with modifiers (kc), the computer's key labels on
// the on-screen keyboard, pointer reports in move order, and Pointer Lock.
import { sleep, keyboardOf, click, centre } from './harness.mjs';
import { PROTOCOL_VERSION } from '../../web/static/js/proto.js';

export const area = {
  name: 'input',
  needs: ['auth'],
  async run(h) {
    const a = h.a;
    await h.sessionOn(a);
    const { press, held } = keyboardOf(a);
    await a.v(`s.record()`);
    const recorded = () => a.v(`JSON.stringify(s.recorded().map((e) => e.t === 'k' ? e.c + (e.d ? '+' : '-') : e.t === 'sync' ? 'sync:' + Object.keys(e.mods).filter((k) => e.mods[k]).join('+') : e.t))`);
    const fresh = () => a.v(`(s.releaseAll(), s.clearRecorded(), s.focus())`);
    // The computer says what it can do, as its hello does.
    await a.v(`s.fromHost({ t: 'hello', version: ${PROTOCOL_VERSION}, caps: ['input', 'relmouse', 'layout', 'chars'] })`);

    // ---- sync ----
    await fresh();
    await a.v(`(document.activeElement.blur(), s.clearRecorded(), s.focus())`);
    await press('ShiftLeft', true); await press('KeyA', true); await press('KeyA', false); await press('ShiftLeft', false);
    h.check('back on the screen, the first key brings the lock and modifier state first (sync), then the keys', await recorded() === JSON.stringify(['sync:shift', 'ShiftLeft+', 'KeyA+', 'KeyA-', 'ShiftLeft-']), await recorded());
    await a.v(`s.clearRecorded()`);
    await press('KeyA', true); await press('KeyA', false);
    h.check('…once: later keys go alone', await recorded() === JSON.stringify(['KeyA+', 'KeyA-']), await recorded());
    await a.v(`s.clearRecorded()`);
    held.add('ControlLeft');
    await press('ControlLeft', false);
    h.check('a modifier let go that never went down here synchronizes instead (Ctrl off)', await recorded() === JSON.stringify(['sync:']), await recorded());
    const full = await a.v(`JSON.stringify(s.recorded()[0])`);
    h.check('sync carries locks and modifiers as the protocol defines them', JSON.parse(full).locks && typeof JSON.parse(full).locks.caps === 'boolean' && Object.keys(JSON.parse(full).mods).sort().join() === 'alt,ctrl,meta,shift', full);

    // ---- Command on a Mac ----
    await fresh();
    await a.v(`s.setMacCommand(true)`);
    await press('MetaLeft', true); await press('KeyC', true);
    held.delete('KeyC');
    await press('MetaLeft', false);
    h.check('on a Mac, keys whose keyup the browser swallows under Command are let go with Command', (await recorded()).endsWith(JSON.stringify(['MetaLeft+', 'KeyC+', 'MetaLeft-', 'KeyC-']).slice(1)) && await a.v(`s.info().pressed.length === 0`), await recorded());
    await a.v(`s.setMacCommand(false)`);
    await fresh();
    await press('MetaLeft', true); await press('KeyC', true); await press('MetaLeft', false);
    h.check('elsewhere, a key still held stays held when Win goes up', (await recorded()).endsWith('"MetaLeft-"]') && await a.v(`s.info().pressed.includes('KeyC')`), await recorded());
    await press('KeyC', false);

    // A system shortcut can consume the physical releases without moving focus from the stage.
    await fresh();
    await a.v(`s.setMacCommand(true)`);
    await press('ControlLeft', true); await press('MetaLeft', true); await press('KeyQ', true);
    held.clear();
    await press('KeyF', true); await press('KeyF', false);
    h.check('after missing Ctrl, Cmd and Q keyups, F goes without modifiers', (await recorded()).endsWith('"ControlLeft-","MetaLeft-","KeyQ-","sync:","KeyF+","KeyF-"]') && await a.v(`s.info().pressed.length === 0`), await recorded());
    await a.v(`s.setMacCommand(false)`);

    // Back from a system overlay, this computer still reports Ctrl and Win down without their keys
    // ever coming here: the sync presses them there. Losing the keyboard lets them go there too.
    await a.v(`(document.activeElement.blur(), s.clearRecorded(), s.focus())`);
    held.add('ControlLeft'); held.add('MetaLeft');
    await press('KeyF', true);
    held.clear();
    await a.v(`(s.clearRecorded(), s.releaseAll())`);
    h.check('modifiers a sync pressed there are let go when the page loses the keyboard', await recorded() === JSON.stringify(['KeyF-', 'ControlLeft-', 'ControlRight-', 'MetaLeft-', 'MetaRight-']), await recorded());

    // ---- the computer's layout on the on-screen keyboard ----
    await fresh();
    await a.v(`s.keyboard(true)`);
    const face = (code) => a.eval(`(() => { const k = document.querySelector('.kb-key[data-code="${code}"]'); return [k.querySelector('.kb-main').textContent, k.querySelector('.kb-alt')?.textContent || ''].join('|'); })()`);
    h.check('before the computer says, the keys show a US keyboard', await face('KeyQ') === 'Q|' && await face('Digit2') === '2|@', await face('Digit2'));
    await a.v(`s.fromHost({ t: 'layout', name: 'fr-FR', labels: { KeyQ: 'a', KeyA: 'q', Digit2: 'é', KeyM: ',' }, shift: { KeyQ: 'A', KeyA: 'Q', Digit2: '2', KeyM: '?' } })`);
    h.check('the computer\'s layout (French): the keys show what they type there', await face('KeyQ') === 'A|' && await face('KeyA') === 'Q|' && await face('Digit2') === 'é|2' && await face('KeyM') === ',|?' && await face('KeyW') === 'W|', [await face('KeyQ'), await face('Digit2'), await face('KeyM')].join(' '));
    h.check('…and are named so', await a.eval(`document.querySelector('.kb-key[data-code="KeyM"]').getAttribute('aria-label') === ','`));

    // ---- a character with modifiers: kc ----
    await click(a, await centre(a, '.kb-key[data-code="ControlLeft"]'));
    await sleep(50);
    await a.v(`s.clearRecorded()`);
    await a.v(`(() => { const t = s.el.typer; t.value = '\\u200b\\u200bç'; t.dispatchEvent(new InputEvent('input', { inputType: 'insertText', data: 'ç' })); })()`);
    const kc = await a.v(`JSON.stringify(s.recorded())`);
    h.check('Ctrl on, then a character from the input method: it goes as that character with Ctrl, for the computer\'s layout (kc)', kc === JSON.stringify([{ t: 'kc', ch: 'ç', mods: { shift: false, ctrl: true, alt: false, meta: false } }]) && await a.v(`Object.keys(s.info().mods).length === 0`), kc);
    await a.v(`s.keyboard(false)`);

    // ---- pointer reports ----
    await a.v(`s.focus()`);
    const box = await a.v(`(() => { const r = s.el.stage.getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })()`);
    await a.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: box.x, y: box.y });
    await sleep(100);
    const seq = await a.v(`s.info().moveSeq`);
    const moves = await a.v(`JSON.stringify(s.recordedMoves().slice(-1))`);
    h.check('mouse moves go numbered (m)', seq > 0 && JSON.parse(moves)[0]?.t === 'm' && JSON.parse(moves)[0].s === seq, moves);
    h.check('when the motion stops, its last move goes once more on the reliable channel, same sequence', await a.v(`JSON.stringify(s.recorded().filter((e) => e.t === 'm'))`) === JSON.stringify(JSON.parse(moves)), await a.v(`JSON.stringify(s.recorded().filter((e) => e.t === 'm'))`));
    // The move channel loses the last move of a motion.
    await a.v(`(s.dropMoves(true), s.clearRecorded())`);
    await a.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: box.x + 40, y: box.y + 20 });
    await sleep(150);
    const lost = await a.v(`s.info().moveSeq`);
    const again = await a.v(`JSON.stringify(s.recorded().filter((e) => e.t === 'm'))`);
    h.check('a move lost on the move channel still reaches the computer: the reliable copy carries it', lost === seq + 1 && JSON.stringify(JSON.parse(again).map((m) => m.s)) === `[${lost}]` && JSON.parse(again)[0].x === JSON.parse(await a.v(`JSON.stringify(s.recordedMoves())`)).at(-1).x, again);
    await a.v(`s.dropMoves(false)`);
    await a.v(`s.fromHost({ t: 'cp', x: 0.4, y: 0.6, v: true, s: ${lost} })`);
    h.check('…so the computer\'s reports, carrying that sequence, are taken again', await a.v(`s.info().pointer.x === 0.4`));
    const seqNow = lost;
    await a.v(`s.setCursor(0.5, 0.5)`);
    await a.v(`s.fromHost({ t: 'cp', x: 0.1, y: 0.1, v: true, s: ${seqNow - 1} })`);
    h.check('a pointer report from before the latest move is ignored', await a.v(`s.info().pointer.x === 0.5`));
    await a.v(`s.fromHost({ t: 'cp', x: 0.25, y: 0.75, v: true, s: ${seqNow} })`);
    h.check('…one that includes it is the pointer\'s place', await a.v(`s.info().pointer.x === 0.25 && s.info().pointer.y === 0.75`));

    // ---- Pointer Lock ----
    h.check('the computer takes relative moves: "Lock pointer" is in the toolbar', await a.v(`!s.el.lockBtn.hidden`));
    await a.v(`s.showToolbar(true)`);
    await sleep(300);
    await click(a, await a.v(`(() => { const r = s.el.lockBtn.getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; })()`));
    await a.until(`s.info().locked`, 'pointer locked', 3000).catch(() => {});
    const locked = await a.v(`s.info().locked && document.pointerLockElement === s.el.stage && s.el.lockBtn.getAttribute('aria-pressed') === 'true'`);
    h.check('"Lock pointer" locks the pointer to the screen', locked);
    if (locked) {
      await a.v(`s.clearRecorded()`);
      for (const [dx, dy] of [[20, 10], [50, 30], [70, 40], [90, 60]]) {
        await a.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: box.x + dx, y: box.y + dy });
        await sleep(40);
      }
      const rel = JSON.parse(await a.v(`JSON.stringify(s.recordedMoves())`));
      h.check('locked, the mouse sends relative moves (mr) in the same sequence; the first motion after locking (a bogus jump in Chrome) is left out', rel.length === 3 && rel.every((m) => m.t === 'mr' && Number.isInteger(m.s) && m.s > seqNow), JSON.stringify(rel));
      if (rel.length === 3) {
        const [first, second, latest] = rel;
        h.check('the locked pointer moves with host reports that arrive behind the latest move', await a.v(`(() => {
          const cursor = s.el.cursor;
          s.fromHost({ t: 'cp', x: 0.35, y: 0.6, v: true, s: ${first.s} });
          const firstPosition = cursor.style.transform;
          const firstShown = !cursor.hidden && s.info().pointer.x === 0.35;
          s.fromHost({ t: 'cp', x: 0.65, y: 0.6, v: true, s: ${second.s} });
          return firstShown && !cursor.hidden && s.info().pointer.x === 0.65 && cursor.style.transform !== firstPosition;
        })()`));
        h.check('the locked pointer ignores reordered reports and follows host visibility', await a.v(`(() => {
          const cursor = s.el.cursor;
          s.fromHost({ t: 'cp', x: 0.2, y: 0.1, v: false, s: ${first.s} });
          const latestKept = !cursor.hidden && s.info().pointer.x === 0.65;
          s.fromHost({ t: 'cp', x: 0.65, y: 0.6, v: false, s: ${latest.s} });
          const hidden = cursor.hidden;
          s.fromHost({ t: 'cp', x: 0.65, y: 0.6, v: true, s: ${latest.s} });
          return latestKept && hidden && !cursor.hidden;
        })()`));
      }
      await a.v(`document.exitPointerLock()`);
      await a.until(`!s.info().locked`, 'pointer unlocked', 3000).catch(() => {});
      h.check('leaving Pointer Lock (Esc) ends the mode and hides the drawn pointer', await a.v(`!s.info().locked && s.el.lockBtn.getAttribute('aria-pressed') === 'false' && s.el.cursor.hidden`));
    }
    await a.v(`s.hideToolbar()`);
    await a.v(`s.fromHost({ t: 'hello', version: ${PROTOCOL_VERSION}, caps: ['input'] })`);
    h.check('a computer without relative moves offers no Pointer Lock', await a.v(`s.el.lockBtn.hidden`));
    await a.v(`(s.releaseAll(), s.stopRecording())`);
  },
};
