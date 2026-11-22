// The shared clipboard in the browser (web/static/js/session/clipboard.js and clipboard-ui.js),
// through the test API: messages as if from the computer, what the page sends recorded, a
// simulated computer on the file channel, and this device's clipboard stubbed (it is never
// touched). The protocol
// against the real host and its real clipboard is the opt-in area `clipboard-host`.
import { sleep, clipboardStub, fakeFileChannel, q } from './harness.mjs';

export const area = {
  name: 'clipboard',
  needs: ['auth'],
  async run(h) {
    const a = h.a;
    await h.sessionOn(a);
    await a.metrics(1440, 900);
    await clipboardStub(a);
    await fakeFileChannel(a);
    await a.v(`(s.fromHost({ t: 'clipshare', on: false }), s.record())`);
    const sent = (t) => a.v(`s.recorded().filter((e) => e.t === '${t}')`);
    const host = (m) => a.v(`s.fromHost(${JSON.stringify(m)})`);
    const lastId = async (t) => (await sent(t)).at(-1)?.id;

    // The toolbar: one word and a dot for the state, also on a narrow screen.
    h.check('the toolbar says Clipboard with a dot that is not lit while sharing is off', await a.v(`s.el.clipBtn.textContent === ${q('clip.name')} && !!s.el.clipBtn.querySelector('.dot') && !s.el.clipBtn.querySelector('.dot.ok')`), await a.v(`s.el.clipBtn.textContent`));
    await a.metrics(390, 844, true);
    await a.v(`s.showToolbar(true)`);
    await sleep(350);
    h.check('on a narrow screen the clipboard button stays one line', await a.v(`(() => { const b = s.el.clipBtn, l = b.querySelector('.tool-clip-label'); return l.getBoundingClientRect().height < 24 && b.scrollWidth <= b.clientWidth + 1; })()`));
    await a.v(`s.hideToolbar()`);
    await a.metrics(1440, 900);

    // Sharing off: nothing of this device's clipboard is read or announced.
    await a.v(`(window.__clip.items = [['text/plain', 'hello']], s.checkClipboard())`);
    await sleep(100);
    h.check('sharing off: a copy here announces nothing and reads nothing', (await sent('cb')).length === 0 && await a.v(`window.__clip.reads === 0`));
    await a.v(`s.togglePanel('clip')`);
    await a.eval(`document.querySelector('.clip .switch').click()`);
    h.check('the switch in the panel asks the computer to share', (await sent('clipshare')).some((e) => e.on === true));
    await host({ t: 'clipshare', on: true });
    h.check('…and once it answers, the toolbar and the panel show sharing on', await a.v(`s.info().share && !!s.el.clipBtn.querySelector('.dot.ok') && document.querySelector('.clip .switch').getAttribute('aria-checked') === 'true'`));
    await a.v(`s.togglePanel('clip')`);

    // A copy here: the formats go, never the content.
    await a.until(`s.recorded().some((e) => e.t === 'cb')`, 'announce after sharing went on', 3000);
    const cb = (await sent('cb'))[0];
    h.check('a copy here announces its formats only: kind and size, no text', JSON.stringify(cb.formats) === JSON.stringify([{ kind: 'text', size: 5 }]) && !('text' in cb), JSON.stringify(cb));
    await a.v(`s.checkClipboard()`);
    await sleep(100);
    h.check('the same clipboard is not announced again', (await sent('cb')).length === 1);
    await host({ t: 'cberr', gen: 2, id: cb.id, why: 'io' });
    await a.v(`s.checkClipboard()`);
    await a.until(`s.recorded().filter((e) => e.t === 'cb').length === 2`, 'announce again after a refusal', 3000);
    h.check('a copy the computer could not place (cberr with its id) is announced again', (await sent('cb'))[1].id !== cb.id);
    const notPlaced = `[...document.querySelectorAll('.notice.dark')].filter((e) => e.textContent.includes(${q('clip.notPlaced')})).length`;
    h.check('…and the person is told it did not get there, not left guessing', await a.eval(notPlaced) === 1);
    await host({ t: 'cberr', gen: 2, id: (await sent('cb'))[1].id, why: 'io' });
    await sleep(100);
    h.check('…once: a second refusal in a row says nothing more', await a.eval(notPlaced) === 1);
    await a.v(`(window.__clip.items = [['text/plain', 'hello'], ['text/html', '<b>hello</b>']], s.checkClipboard())`);
    await a.until(`s.recorded().filter((e) => e.t === 'cb').length === 3`, 'second announce', 3000);
    h.check('a new copy is announced again, with every format it has', JSON.stringify((await sent('cb'))[2].formats.map((f) => f.kind)) === '["text","html"]');

    // A paste on the computer asks for one format of the browser's latest generation.
    await host({ t: 'cb', gen: 3, owner: 'browser', formats: [{ kind: 'text', size: 5 }, { kind: 'html', size: 12 }] });
    await host({ t: 'cbreq', gen: 2, format: 'text', id: 'old' });
    await a.until(`s.recorded().some((e) => e.t === 'cberr')`, 'refusal of an old generation', 3000);
    h.check('a request for an older generation is refused: gone', (await sent('cberr'))[0].why === 'gone');
    await host({ t: 'cbreq', gen: 3, format: 'html', id: 'p1' });
    await a.until(`s.recorded().some((e) => e.t === 'cbdata')`, 'answer to a paste', 3000);
    const data = (await sent('cbdata'))[0];
    h.check('a paste there gets the data of the format asked, inline when small', data.gen === 3 && data.id === 'p1' && data.html === '<b>hello</b>' && !('text' in data), JSON.stringify(data));

    // Large data goes on the file channel as a transfer under the request, with progress.
    await a.v(`(window.__files.delay = 900, window.__clip.items = [['image/png', new Blob([new Uint8Array(100000)], { type: 'image/png' })]], s.checkClipboard())`);
    await a.until(`s.recorded().filter((e) => e.t === 'cb').length === 4`, 'image announce', 3000);
    await host({ t: 'cb', gen: 4, owner: 'browser', formats: [{ kind: 'image', size: 100000 }] });
    await host({ t: 'cbreq', gen: 4, format: 'image', id: 'p2' });
    await a.until(`!!s.el.clipNote && s.el.clipNote.textContent.includes(${q('clip.pasting')})`, 'paste note', 3000);
    h.check('a slow paste shows "Pasting on the remote computer…" with ✕ at the top right', await a.v(`!!s.el.clipNote.querySelector('.x-btn') && s.el.stack.contains(s.el.clipNote)`));
    await a.until(`!s.info().clip.serving`, 'image paste done', 5000);
    const put = await a.v(`window.__files.sent.find((m) => m.t === 'put')`);
    h.check('the picture went as a put of kind image with the request id and generation', put.id === 'p2' && put.gen === 4 && put.kind === 'image' && (await sent('cbdata')).at(-1).size === 100000, JSON.stringify(put));
    h.check('…and the note goes away', await a.v(`!s.el.clipNote`));
    await a.v(`window.__files.delay = 0`);

    // ✕: the paste gets nothing.
    await a.v(`window.__clip.hangFrom = window.__clip.reads + 1`);
    await host({ t: 'cbreq', gen: 4, format: 'image', id: 'p3' });
    await a.until(`!!s.el.clipNote`, 'note of a slow read', 3000);
    await a.v(`s.el.clipNote.querySelector('.x-btn').click()`);
    const cancelled = (await sent('cberr')).find((e) => e.id === 'p3');
    h.check('✕ cancels the paste: cberr cancelled, and the note goes', cancelled?.why === 'cancelled' && await a.v(`!s.el.clipNote && !s.info().clip.serving`), JSON.stringify(cancelled));
    await a.v(`window.__clip.hangFrom = Infinity`);

    // Copied on the computer: text comes onto this device's clipboard at once.
    await host({ t: 'cb', gen: 5, owner: 'host', formats: [{ kind: 'text', size: 11 }], text: 'from remote' });
    await a.until(`window.__clip.writes.some((w) => w[1] === 'from remote')`, 'remote text written here', 3000);
    h.check('text copied there is on this device’s clipboard at once', true);
    await a.v(`(window.__clip.items = [['text/plain', 'from remote']], s.clearRecorded(), s.checkClipboard())`);
    await sleep(150);
    h.check('…and is not announced back as a copy here', (await sent('cb')).length === 0);
    await host({ t: 'cb', gen: 4, owner: 'host', formats: [{ kind: 'text' }], text: 'stale' });
    h.check('an older generation changes nothing', await a.v(`s.info().clip.gen === 5 && !window.__clip.writes.some((w) => w[1] === 'stale')`));

    // Text too long to come with the announcement waits like a picture: nothing is asked for
    // until "Get it here", then it comes over and is written here.
    await a.v(`s.clearRecorded()`);
    await host({ t: 'cb', gen: 6, owner: 'host', formats: [{ kind: 'text', size: 60000 }] });
    await sleep(500);
    h.check('long text copied there: nothing is transferred, the button counts it', (await sent('cbreq')).length === 0 && await a.v(`s.info().offered && !!s.el.clipBtn.querySelector('.tool-badge')`));
    await a.v(`s.togglePanel('clip')`);
    h.check('…the panel describes it with Get it here', await a.eval(`document.querySelector('.clip-desc').textContent.includes(${q('clip.kind.text')}) && [...document.querySelectorAll('.clip-item button')].map((b) => b.textContent).join() === ${q('clip.take')}`), await a.eval(`document.querySelector('.clip').textContent`));
    await a.eval(`document.querySelector('.clip-item button').click()`);
    await a.v(`s.togglePanel('clip')`);
    await a.until(`s.recorded().some((e) => e.t === 'cbreq' && e.gen === 6 && e.format === 'text')`, 'request for long text after Get it here', 3000);
    const big = 'x'.repeat(60000);
    let id = await lastId('cbreq');
    await host({ t: 'cbdata', gen: 6, format: 'text', id, size: 60000 });
    await a.v(`(() => { const id = ${JSON.stringify(id)}; s.fromFiles(JSON.stringify({ t: 'meta', id, name: 'clipboard.txt', size: 60000, mime: 'text/plain', gen: 6 })); const b = new TextEncoder().encode('x'.repeat(60000)); for (let o = 0; o < b.length; o += 16384) s.fromFiles(b.slice(o, o + 16384).buffer); s.fromFiles(JSON.stringify({ t: 'fin', id })); })()`);
    await a.until(`window.__clip.writes.some((w) => w[1] && w[1].length === 60000)`, 'large text written', 3000);
    h.check('…Get it here brings it over the file channel and writes it here', (await a.v(`window.__clip.writes.at(-1)[1]`)) === big);

    // A picture copied there: a description and "Get it here", nothing transferred before that.
    await a.v(`s.clearRecorded()`);
    await host({ t: 'cb', gen: 7, owner: 'host', formats: [{ kind: 'image', w: 1920, h: 1080, size: 300 }] });
    h.check('a picture copied there waits in Clipboard: the button counts it, nothing over the screen, nothing asked for', await a.v(`s.info().offered && !!s.el.clipBtn.querySelector('.tool-badge') && ![...s.el.stack.children].some((n) => !n.hidden && n.textContent.includes(${q('clip.take')}))`) && (await sent('cbreq')).length === 0);
    await a.v(`s.togglePanel('clip')`);
    h.check('the panel describes it with one button: Get it here', await a.eval(`document.querySelector('.clip-desc').textContent.includes('1920×1080') && [...document.querySelectorAll('.clip-item button')].map((b) => b.textContent).join() === ${q('clip.take')}`), await a.eval(`document.querySelector('.clip').textContent`));
    await a.shot('clipboard-panel', 1440, 900);
    await a.metrics(1440, 900);
    await a.eval(`document.querySelector('.clip-item button').click()`);
    const req = (await sent('cbreq'))[0];
    h.check('Get it here asks for the picture of that generation', req.gen === 7 && req.format === 'image', JSON.stringify(req));
    await host({ t: 'cbdata', gen: 7, format: 'image', id: req.id, size: 300 });
    await a.v(`(() => { const id = ${JSON.stringify(req.id)}; s.fromFiles(JSON.stringify({ t: 'meta', id, name: 'image.png', size: 300, mime: 'image/png', gen: 7 })); s.fromFiles(new Uint8Array(300).buffer); s.fromFiles(JSON.stringify({ t: 'fin', id })); })()`);
    await a.until(`window.__clip.writes.some((w) => w[0] === 'image' && w[1] === 300)`, 'picture on this device’s clipboard', 3000);
    h.check('…and puts it on this device’s clipboard', await a.v(`!s.el.clipBtn.querySelector('.tool-badge')`));
    await a.v(`s.togglePanel('clip')`);

    // Files copied there: each one into this device's downloads.
    await a.v(`s.clearRecorded()`);
    await host({ t: 'cb', gen: 8, owner: 'host', formats: [{ kind: 'files', items: [{ n: 'r.bin', s: 4000 }, { n: 'b.txt', s: 10 }] }, { kind: 'text', size: 20 }], text: 'C:\\\\r.bin\\r\\nC:\\\\b.txt' });
    h.check('files copied there: their paths are on this device’s clipboard as text, the files wait', await a.v(`window.__clip.writes.at(-1)[1].includes('r.bin') && s.info().offered`) && (await sent('cbreq')).length === 0);
    await a.v(`s.takeFromRemote()`);
    const reqs = await sent('cbreq');
    h.check('Get it here on files asks for each of them', reqs.length === 2 && reqs.map((r) => r.i).join() === '0,1' && reqs.every((r) => r.format === 'files' && r.gen === 8), JSON.stringify(reqs));
    for (const [r, n] of [[reqs[0], 4000], [reqs[1], 10]]) {
      await a.v(`(() => { const id = ${JSON.stringify(r.id)}; s.fromFiles(JSON.stringify({ t: 'meta', id, name: 'f', size: ${n}, gen: 8 })); s.fromFiles(new Uint8Array(${n}).buffer); s.fromFiles(JSON.stringify({ t: 'fin', id })); })()`);
    }
    await a.until(`window.__blobs.includes(4000) && window.__blobs.includes(10)`, 'both files downloaded', 3000);
    h.check('…and saves each into the downloads', true);

    // Taking can be cancelled: the computer is told on both channels.
    await a.v(`(s.clearRecorded(), window.__files.sent.length = 0, s.takeFromRemote())`);
    await a.until(`!!s.el.clipNote && s.el.clipNote.textContent.includes('r.bin')`, 'note while taking', 3000);
    await a.v(`s.el.clipNote.querySelector('.x-btn').click()`);
    const gaveUp = (await sent('cberr'))[0];
    h.check('✕ while taking: cberr cancelled and the file channel cancel', gaveUp?.why === 'cancelled' && await a.v(`window.__files.sent.some((m) => m.t === 'cancel' && m.id === ${JSON.stringify(gaveUp?.id)})`), JSON.stringify(gaveUp));
    await a.v(`(() => { while (s.el.clipNote) s.el.clipNote.querySelector('.x-btn').click(); })()`);

    // Sharing off: what was offered goes.
    await host({ t: 'clipshare', on: false });
    h.check('sharing off forgets what the computer copied', await a.v(`!s.info().offered && !s.info().clip.gen && !s.el.clipBtn.querySelector('.dot.ok')`));

    // A paste may lose clipboard access after its formats were already announced.
    await a.v(`(s.clearRecorded(), window.__clip.items = [['text/plain', 'same copy']])`);
    await host({ t: 'clipshare', on: true });
    await a.until(`s.recorded().some((e) => e.t === 'cb')`, 'announce before losing access');
    const formats = (await sent('cb'))[0].formats;
    await host({ t: 'cb', gen: 9, owner: 'browser', formats });
    const deniedNotes = `[...document.querySelectorAll('.notice.dark')].filter((e) => e.textContent.includes(${q('clip.readDenied')})).length`;
    const previousNotes = await a.eval(deniedNotes);
    await a.v(`window.__clip.deny = true`);
    await host({ t: 'cbreq', gen: 9, format: 'text', id: 'unfocused-paste' });
    await a.until(`s.recorded().some((e) => e.t === 'cberr' && e.id === 'unfocused-paste')`, 'paste denied while clipboard access is lost');
    await host({ t: 'cbreq', gen: 9, format: 'text', id: 'still-unfocused' });
    await a.until(`s.recorded().some((e) => e.t === 'cberr' && e.id === 'still-unfocused')`, 'another paste denied');
    h.check('repeated denied paste requests show just one notice', await a.eval(deniedNotes) === previousNotes + 1);
    await a.v(`(window.__clip.deny = false, window.dispatchEvent(new Event('focus')))`);
    await a.until(`s.recorded().filter((e) => e.t === 'cb').length === 2`, 'same copy republished after clipboard access returns');
    await host({ t: 'cb', gen: 10, owner: 'browser', formats });
    await host({ t: 'cbreq', gen: 10, format: 'text', id: 'retry-paste' });
    await a.until(`s.recorded().some((e) => e.t === 'cbdata' && e.id === 'retry-paste')`, 'next paste receives the same copy');
    h.check('a temporarily denied paste works again without copying on the controller', (await sent('cbdata')).at(-1)?.text === 'same copy');
    await host({ t: 'clipshare', on: false });

    await a.v(`(s.useFileChannel(null), s.stopRecording())`);
    await sleep(100);
  },
};
