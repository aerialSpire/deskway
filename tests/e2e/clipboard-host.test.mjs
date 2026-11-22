// The shared clipboard against the real host, with the test computer's REAL clipboard: the
// protocol end to end (sharing off, the switch in the session and in the panel, announce,
// request, data, large data, cancel, generations). Only this device's (the browser's)
// clipboard is stubbed. Opt-in, because it uses the clipboard of the computer running the
// tests: run it by name (`--areas clipboard-host`). What is on that clipboard (text, a
// picture or files) is saved first and put back afterwards.
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { sleep, clipboardStub, q } from './harness.mjs';

const ps = (cmd) => spawnSync('powershell', ['-NoProfile', '-NonInteractive', '-Command', cmd], { encoding: 'utf8', windowsHide: true }).stdout.replace(/\r?\n$/, '');
// A paste on the test computer, as another program does it; returns when that program has its data.
const pasteThere = () => new Promise((resolve) => {
  const p = spawn('powershell', ['-NoProfile', '-NonInteractive', '-Command', '[Console]::OutputEncoding = [Text.Encoding]::UTF8; Get-Clipboard -Raw'], { stdio: ['ignore', 'pipe', 'ignore'], windowsHide: true });
  let out = '';
  p.stdout.on('data', (d) => { out += d; });
  p.on('close', () => resolve(out.replace(/\r?\n$/, '')));
});

// The computer's clipboard as it was: text, a picture or a file list, kept in `dir`.
const saveClipboard = (dir) => ps(`Add-Type -AssemblyName System.Windows.Forms; $c = [Windows.Forms.Clipboard]; if ($c::ContainsFileDropList()) { [IO.File]::WriteAllLines('${dir}/files.txt', [string[]]$c::GetFileDropList()) } elseif ($c::ContainsImage()) { $c::GetImage().Save('${dir}/image.png') } elseif ($c::ContainsText()) { [IO.File]::WriteAllText('${dir}/text.txt', $c::GetText()) }`);
const restoreClipboard = (dir) => ps(`Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; $c = [Windows.Forms.Clipboard]; if (Test-Path '${dir}/files.txt') { $l = New-Object Collections.Specialized.StringCollection; $l.AddRange([IO.File]::ReadAllLines('${dir}/files.txt')); $c::SetFileDropList($l) } elseif (Test-Path '${dir}/image.png') { $i = [Drawing.Image]::FromFile('${dir}/image.png'); $c::SetImage($i); $i.Dispose() } elseif (Test-Path '${dir}/text.txt') { $c::SetText([IO.File]::ReadAllText('${dir}/text.txt')) } else { $c::Clear() }`);

export const area = {
  name: 'clipboard-host',
  needs: ['auth'],
  async run(h) {
    const dir = mkdtempSync(join(tmpdir(), 'deskway-clip-'));
    const keep = mkdtempSync(join(tmpdir(), 'deskway-clip-saved-'));
    saveClipboard(keep);
    const panel = h.panelPipe();
    const a = h.a;
    try {
      await h.sessionOn(a);
      await clipboardStub(a);
      // Every message from the computer is noted on its way in.
      await a.v(`s.hearHost()`);
      const said = (t) => a.v(`s.heard().filter((m) => m.t === '${t}')`);
      const ask = (m) => a.v(`s.toHost(${JSON.stringify(m)})`);
      const latest = async (owner) => (await said('cb')).filter((m) => m.owner === owner).at(-1);
      // The computer's setting starts off.
      if (await a.v(`s.info().share`)) { await ask({ t: 'clipshare', on: false }); await a.until(`!s.info().share`, 'sharing off first'); }

      await ask({ t: 'cbreq', gen: 0, format: 'text', id: 'r0' });
      await a.until(`s.heard().some((m) => m.t === 'cberr' && m.id === 'r0')`, 'answer while off');
      h.check('real host, sharing off: a request is refused (denied) and nothing is announced', (await said('cberr'))[0].why === 'denied' && (await said('cb')).length === 0);

      // The switch in the session's panel: the computer saves its setting and says so.
      await a.v(`s.togglePanel('clip')`);
      await a.eval(`document.querySelector('.clip .switch').click()`);
      await a.until(`s.info().share`, 'sharing on from the session');
      await a.v(`s.togglePanel('clip')`);
      await sleep(300);
      h.check('real host: the session switch turns sharing on, and the computer’s panel shows it', panel.statuses.at(-1)?.clip_share === true, JSON.stringify(panel.statuses.at(-1)?.clip_share));

      // Copied on the computer: announced with a new generation, small text inline.
      await a.v(`s.hearHost()`);
      ps(`Set-Clipboard -Value 'deskway e2e from the computer'`);
      await a.until(`window.__clip.writes.some((w) => w[1] === 'deskway e2e from the computer')`, 'copy there written here', 8000);
      const fromHost = await latest('host');
      h.check('real host: a copy there is announced (owner host, text inline) and lands on this device’s clipboard', fromHost?.gen > 0 && fromHost.text === 'deskway e2e from the computer', JSON.stringify(fromHost));
      await ask({ t: 'cbreq', gen: fromHost.gen, format: 'text', id: 'r1' });
      await ask({ t: 'cbreq', gen: fromHost.gen - 1, format: 'text', id: 'r2' });
      await a.until(`s.heard().some((m) => m.id === 'r1') && s.heard().some((m) => m.id === 'r2')`, 'answers to requests');
      const r1 = (await said('cbdata')).find((m) => m.id === 'r1');
      const r2 = (await said('cberr')).find((m) => m.id === 'r2');
      h.check('real host: a request for the current generation gets the data, an older one is gone', r1?.text === 'deskway e2e from the computer' && r2?.why === 'gone', JSON.stringify([r1, r2]));

      // Files copied there: names and sizes come; the bytes only when asked.
      const file = join(dir, 'deskway e2e.txt');
      writeFileSync(file, 'x'.repeat(70000));
      ps(`Set-Clipboard -Path '${file.replace(/'/g, "''")}'`);
      await a.until(`s.info().offered`, 'files offered', 8000);
      const files = await latest('host');
      h.check('real host: files copied there are announced with their names and sizes, and their paths as text', JSON.stringify(files.formats.find((f) => f.kind === 'files')?.items) === JSON.stringify([{ n: 'deskway e2e.txt', s: 70000 }]) && files.text === file, JSON.stringify(files));
      await a.v(`s.takeFromRemote()`);
      await a.until(`window.__blobs.includes(70000)`, 'file taken', 8000);
      h.check('real host: Get it here brings the file over the file channel', true);

      // Copied here: the computer gets placeholders and asks when something pastes there.
      await a.v(`(window.__clip.items = [['text/plain', 'deskway e2e from the browser']], s.checkClipboard())`);
      await a.until(`s.heard().some((m) => m.t === 'cb' && m.owner === 'browser')`, 'browser copy acknowledged', 5000);
      const mine = await latest('browser');
      h.check('real host: a copy here gets the next generation, owner browser', mine.gen > files.gen, JSON.stringify(mine));
      const pasted = await pasteThere();
      h.check('real host: a paste there asks this page and gets the text', pasted === 'deskway e2e from the browser', JSON.stringify(pasted));
      const asked = (await said('cbreq')).filter((m) => m.gen === mine.gen);
      h.check('real host: the paste asked for the browser’s generation and format', asked.length >= 1 && asked.every((m) => m.format === 'text'), JSON.stringify(asked));

      // Large text: the bytes go as a transfer under the request (also to a view-only host).
      const big = 'y'.repeat(60000);
      await a.v(`(window.__clip.items = [['text/plain', ${JSON.stringify(big)}]], s.checkClipboard())`);
      await a.until(`s.heard().filter((m) => m.t === 'cb' && m.owner === 'browser').length === 2`, 'large copy acknowledged', 5000);
      const pastedBig = await pasteThere();
      h.check('real host: 60 KB of text pastes there, sent on the file channel', pastedBig === big, `${pastedBig.length} characters`);

      // Cancelled: the paste there gets nothing, at once.
      await a.v(`(window.__clip.items = [['text/plain', 'deskway e2e never']], window.__clip.hangFrom = window.__clip.reads + 2, s.checkClipboard())`);
      await a.until(`s.heard().filter((m) => m.t === 'cb' && m.owner === 'browser').length === 3`, 'third copy acknowledged', 5000);
      const started = Date.now();
      const pending = pasteThere();
      await a.until(`!!s.el.clipNote && s.el.clipNote.textContent.includes(${q('clip.pasting')})`, 'paste note on a slow read', 8000);
      await a.v(`s.el.clipNote.querySelector('.x-btn').click()`);
      const cancelledPaste = await pending;
      h.check('real host: ✕ answers the paste there with nothing, without waiting for the deadline', cancelledPaste === '' && Date.now() - started < 15000, JSON.stringify([cancelledPaste, Date.now() - started]));

      // The switch in the computer's panel: off, the session hears it and the placeholders go.
      await a.v(`(window.__clip.hangFrom = Infinity, window.__clip.items = [['text/plain', 'deskway e2e last']], s.checkClipboard())`);
      await a.until(`s.heard().filter((m) => m.t === 'cb' && m.owner === 'browser').length === 4`, 'last copy acknowledged', 5000);
      panel.send({ ClipShare: false });
      await a.until(`!s.info().share`, 'sharing off from the panel', 5000);
      await sleep(300);
      const after = await pasteThere();
      h.check('real host: the panel switch turns sharing off in the session, and the browser’s placeholders are gone', after !== 'deskway e2e last' && panel.statuses.at(-1)?.clip_share === false, JSON.stringify(after));
    } finally {
      panel.send({ ClipShare: false });
      panel.close();
      restoreClipboard(keep);
      rmSync(dir, { recursive: true, force: true });
      rmSync(keep, { recursive: true, force: true });
    }
  },
};
