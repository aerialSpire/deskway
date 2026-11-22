// Files and pictures to the computer: the real file channel first, then a simulated computer
// answering on it; quick and slow transfers, dropping, cancelling, stalled transfers. (What
// the shared clipboard does with the file channel is in clipboard.test.mjs.)
import { sleep, fakeFileChannel, q } from './harness.mjs';

export const area = {
  name: 'transfer',
  needs: ['auth'],
  async run(h) {
    const a = h.a;
    await h.sessionOn(a);
    await a.metrics(1440, 900);
    // The real path first: the test host is view only, so it says it does not take files.
    await a.until(`s.info().files === 'open'`, 'file channel open');
    await a.v(`s.sendFiles([new File(['hello'], 'a.txt')])`);
    await a.until(`s.el.xferCard && s.el.xferCard.textContent.includes(${q('xfer.why.unsupported')})`, 'unsupported answer from the host', 8000);
    h.check('files: the host answers on the file channel (this view-only host takes none)', true);
    await a.v(`s.dismissTransfer()`);
    await a.v(`s.sendFiles([new File([new Uint8Array(26 << 20)], 'big.bin')])`);
    h.check('files: more than 25 MB is refused at once, nothing sent', await a.v(`s.el.xferCard.textContent.includes(${q('xfer.tooBig')}) && !s.info().transferring`));
    await a.v(`s.dismissTransfer()`);

    // From here a simulated computer answers, and input is recorded instead of sent.
    await fakeFileChannel(a);
    await a.v(`s.record()`);
    const fake = (expr) => a.eval(`(() => { const f = window.__files; return ${expr}; })()`);
    // A quick transfer shows nothing at all.
    await a.v(`s.sendFiles([new File([new Uint8Array(3000)], 'quick.bin')])`);
    await a.until(`!s.info().transferring && window.__files.bin === 3000`, 'quick transfer done', 4000);
    h.check('a transfer under half a second shows no card', await a.v(`!s.el.xferCard`));

    // A slower one: one small line with what goes, how far, and ✕. The computer's desktop is the
    // unlocked one here (the test computer may be locked: its note would sit above the line).
    await a.v(`s.fromHost({ t: 'desk', v: 'default' })`);
    await a.eval(`window.__files.delay = 1500`);
    await a.v(`s.sendFiles([new File(['report'], '报告.pdf'), new File(['data'], 'data.xlsx')])`);
    await a.until(`s.el.xferCard && s.el.xferCard.textContent.includes(${q('xfer.files', { n: 2, name: '报告.pdf' })})`, 'card for a slower transfer', 3000);
    h.check('a transfer that takes a moment shows one small line: what, progress and ✕', await a.v(`(() => { const c = s.el.xferCard, r = c.getBoundingClientRect(), st = s.el.stage.getBoundingClientRect(), t = c.textContent; return r.height <= 30 && r.width <= 360 && r.top - st.top <= 32 && st.right - r.right <= 12 && t.includes(${q('xfer.files', { n: 2, name: '报告.pdf' })}) && t.includes('%') && !!c.querySelector('.x-btn') && !t.includes('null'); })()`), await a.v(`(() => { const c = s.el.xferCard, r = c.getBoundingClientRect(), st = s.el.stage.getBoundingClientRect(); return c.textContent + ' ' + JSON.stringify({ h: r.height, w: r.width, top: r.top - st.top, right: st.right - r.right, above: [...c.parentElement.children].filter((e) => e !== c && e.getClientRects().length).map((e) => e.className + ':' + e.textContent.slice(0, 40)) }); })()`));
    await a.shot('transfer-card', 1440, 900);
    await a.metrics(1440, 900);
    await a.until(`!s.info().transferring`, 'slower transfer done', 6000);

    // Dropped on the screen: sent, never pasted by itself.
    await a.eval(`window.__files.delay = 0`);
    await a.v(`(s.clearRecorded(), s.dismissTransfer())`);
    await a.v(`(() => { const dt = new DataTransfer(); dt.items.add(new File(['x'], 'dropped.txt')); s.el.stage.dispatchEvent(new DragEvent('dragover', { dataTransfer: dt, bubbles: true, cancelable: true })); })()`);
    h.check('dragging files over the screen says to let go', await a.v(`s.el.stage.classList.contains('dropping')`));
    await a.v(`(() => { const dt = new DataTransfer(); dt.items.add(new File(['x'], 'dropped.txt')); s.el.stage.dispatchEvent(new DragEvent('drop', { dataTransfer: dt, bubbles: true, cancelable: true })); })()`);
    await a.until(`window.__files.put.items[0].n === 'dropped.txt' && !s.info().transferring`, 'dropped file sent', 3000);
    h.check('a dropped file is sent and not pasted by itself', await a.v(`!s.recorded().some((e) => e.t === 'k') && !s.el.stage.classList.contains('dropping')`));

    // Cancel: the host is told, the card goes away.
    await a.eval(`window.__files.delay = 5000`);
    await a.v(`s.sendFiles([new File(['slow'], 'slow.txt')])`);
    await a.until(`!!s.el.xferCard`, 'card before cancelling', 2000);
    await a.v(`s.el.xferCard.querySelector('.x-btn').click()`);
    h.check('cancelling tells the host and removes the card', await fake(`f.sent.some((m) => m.t === 'cancel' && m.id === f.put.id)`) && await a.v(`!s.el.xferCard && !s.info().transferring`));

    // A transfer that stops moving ends with a message, and the next one goes.
    await a.v(`(s.setStall(1500), window.__files.delay = 0, window.__files.silent = true, window.__files.sent.length = 0, s.dismissTransfer())`);
    await a.v(`s.sendFiles([new File(['stuck'], 'stuck.txt')])`);
    await a.until(`s.el.xferCard && s.el.xferCard.textContent.includes(${q('xfer.why.timeout')})`, 'stalled transfer ended', 8000);
    h.check('a stalled transfer ends by itself, tells the computer to stop, and says so', await fake(`f.sent.some((m) => m.t === 'cancel')`) && await a.v(`!s.info().transferring`));
    await a.v(`(window.__files.silent = false, s.setStall(20000), s.dismissTransfer())`);
    await a.v(`s.sendFiles([new File(['next'], 'next.txt')])`);
    await a.until(`window.__files.put.items[0].n === 'next.txt' && !s.info().transferring`, 'next transfer after a stalled one', 5000);
    h.check('…and the next transfer goes through', true);

    // Back to the real channel and input.
    await a.v(`(s.useFileChannel(null), s.stopRecording(), s.dismissTransfer())`);
    await sleep(100);
  },
};
