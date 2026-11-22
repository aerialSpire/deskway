// An AI client on the test computer, as a person and their AI client meet it
// (docs/architecture.md, "AI control"): `deskway login`, without the password, is refused until "Add an AI
// client" has copied its prompt (the program to download, with its SHA-256, as the server has it);
// then it waits, and that page asks for it by name with the digits the client shows; signed out,
// it signs in again with its key alone; "My devices" tags it AI and
// gives the computer an "Allow AI control" switch, off until the person turns it on; a call
// before that is refused (ai_off) and after it is answered; the page says the AI client is
// working; turning the switch off ends it again. What each side decides is tested where it
// is decided (auth, signal, the server's tests); this area checks only what a person sees and
// that the pieces meet.
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { createHash } from 'node:crypto';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { sleep, clipboardStub, t, q, fixed } from './harness.mjs';

const exeOf = (h) => join(dirname(h.opts.hostExe), process.platform === 'win32' ? 'deskway.exe' : 'deskway');

// Runs deskway with its own folder; resolves with its exit code and what it printed. One that
// takes longer than `ms` is ended, and says so.
function deskway(h, home, args, input = '', ms = 120000) {
  const p = spawn(exeOf(h), args, { env: { ...process.env, DESKWAY_CLI_HOME: home }, windowsHide: true });
  const out = { text: '', code: null };
  p.stdout.on('data', (d) => { out.text += d; });
  p.stderr.on('data', (d) => { out.text += d; });
  const timer = setTimeout(() => { out.text += `
(ended after ${ms} ms)`; p.kill(); }, ms);
  out.done = new Promise((res) => p.on('close', (code) => { clearTimeout(timer); out.code = code; res(out); }));
  p.stdin.end(input);
  return out;
}

// An AI client's MCP server kept open, as an AI client runs it while it works: what holds a
// computer. `call` sends one tool call and waits for its answer; `close` ends it as a client
// closing would.
function mcpOpen(h, home) {
  const p = spawn(exeOf(h), ['mcp'], { env: { ...process.env, DESKWAY_CLI_HOME: home }, windowsHide: true });
  let out = '';
  let id = 0;
  p.stdout.on('data', (d) => { out += d; });
  const call = async (name, args) => {
    id += 1;
    p.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method: 'tools/call', params: { name, arguments: args } })}\n`);
    return waitFor(() => out.split('\n').map((l) => { try { return JSON.parse(l); } catch { return null; } }).find((m) => m && m.id === id), `the answer to ${name}`, 60000);
  };
  const close = () => new Promise((res) => { p.on('close', res); p.stdin.end(); });
  return { call, close };
}

async function waitFor(f, label, ms = 30000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const v = await f();
    if (v) return v;
    await sleep(200);
  }
  throw new Error(`timed out waiting for: ${label}`);
}

export const area = {
  name: 'ai',
  needs: ['auth'],
  async run(h) {
    const a = h.a;
    const home = mkdtempSync(join(tmpdir(), 'deskway-ai-'));
    const computer = await a.v(`d.devices().find((x) => x.id === ${JSON.stringify(h.hostId)}).name`);

    const signIn = () => deskway(h, home, ['login', '--server', h.opts.url, '--user', h.opts.user, '--name', 'Claude E2E']);

    // Before anyone opened the account to AI sign-ins, one is refused, and says where to go.
    const early = await signIn().done;
    h.check('an AI sign-in before the prompt was copied is refused, saying where to go', early.code !== 0 && early.text.includes('Add an AI client'), early.text.trim());

    // The server hands out the program it has, here the one this test built, as Windows x64.
    const exe = join(dirname(h.opts.hostExe), process.platform === 'win32' ? 'deskway.exe' : 'deskway');
    const clients = join(dirname(h.opts.serverState), 'clients');
    mkdirSync(clients, { recursive: true });
    copyFileSync(exe, join(clients, 'deskway-windows-x64.exe'));
    const sha = createHash('sha256').update(readFileSync(exe)).digest('hex');

    // "Add an AI client", from "My devices": the prompt names this server, the account and the program
    // with its hash; copying it starts the 10 minutes. This computer's clipboard is stubbed.
    await a.v(`d.show('devices')`);
    await a.until(`!!document.querySelector('.device-add')`, 'Add an AI client in My devices');
    await a.eval(`document.querySelector('.device-add').click()`);
    await a.until(`!!document.querySelector('.ai-prompt-text')`, 'the prompt');
    const promptText = await a.eval(`document.querySelector('.ai-prompt-text').textContent`);
    h.check('the prompt names the server, the account, the program and its SHA-256, and no password',
      promptText.includes(`--server ${h.opts.url}`) && promptText.includes(`--user ${h.opts.user}`) && promptText.includes(`${h.opts.url}/download/deskway-windows-x64.exe`) && promptText.includes(sha) && !promptText.includes(h.opts.password), promptText.slice(0, 200));
    h.check('before copying, the page says the 10 minutes start with it', await a.eval(`!document.querySelector('.ai-wait')`));
    await a.shot('web-ai-client', 1440, 900);
    await a.shot('web-ai-client-phone', 390, 844);
    await a.metrics(1440, 900);
    const served = await (await fetch(`${h.opts.url}/download/deskway-windows-x64.exe`)).arrayBuffer();
    h.check('the program downloads as it is listed', createHash('sha256').update(Buffer.from(served)).digest('hex') === sha);
    await clipboardStub(a);
    await a.clickText('.ai-prompt-bar button', t('aiclient.copy'), { exact: true });
    await a.until(`!!document.querySelector('.ai-wait')`, 'waiting for the AI to sign in');
    h.check('copying puts the prompt on the clipboard and starts the 10 minutes', await a.eval(`window.__clip.writes.some(([k, t]) => k === 'text' && t === document.querySelector('.ai-prompt-text').textContent) && /9:5\\d|10:00/.test(document.querySelector('.ai-wait').textContent)`));

    // Signing in: the client shows six digits and waits; the page asks with the same digits, in
    // place, not in a dialog.
    const login = signIn();
    const digits = await waitFor(() => (login.text.match(/numbers (\d{3}) (\d{3})/) || []).slice(1).join(''), 'the client\'s digits');
    await a.until(`!!document.querySelector('.ai-request .code')`, 'the AI client asking on the page');
    const shown = await a.eval(`document.querySelector('.ai-request .code').textContent.replace(/\\D/g, '')`);
    h.check('the page asks for the AI client by name, with the digits the client shows, in place', shown === digits && !(await a.eval(`!!document.querySelector('.dialog')`)) && (await a.eval(`document.querySelector('.ai-request').textContent.includes('Claude E2E')`)), `${shown} / ${digits}`);
    await a.shot('web-ai-approve', 1440, 900);
    await a.shot('web-ai-approve-phone', 390, 844);
    await a.metrics(1440, 900);
    await a.clickText('.ai-request button', t('common.approve'), { exact: true });
    await login.done;
    h.check('the client is signed in once approved', login.code === 0, login.text.trim().split('\n').pop());
    await a.until(`document.querySelector('.dialog h2')?.textContent.includes(${q('approve.addedTitle')})`, 'added');
    await a.clickText('.dialog button', t('common.ok'));

    // Signed out, it signs in again with its key alone: nobody approves, no window needed.
    await deskway(h, home, ['logout']).done;
    const again = await signIn().done;
    h.check('an approved AI client signs in again with its key alone', again.code === 0 && !again.text.includes('numbers'), again.text.trim());

    // "My devices": the AI client tagged, the computer's switch off.
    await a.v(`d.show('devices')`);
    await a.until(`[...document.querySelectorAll('.device-row')].some((r) => r.textContent.includes('Claude E2E') && r.textContent.includes('AI'))`, 'the AI client in Controllers');
    h.check('My devices lists the AI client, tagged AI', true);
    await a.shot('web-devices-ai-off', 1440, 900);
    const sw = `document.querySelector('[data-ai=${JSON.stringify(h.hostId)}]')`;
    h.check('the computer has an Allow AI control switch, off', await a.eval(`${sw}?.getAttribute('aria-checked') === 'false'`));

    // This AI client runs on the test computer, and its list of computers says which one that is.
    const mine = await waitFor(async () => {
      const o = await deskway(h, home, ['computers']).done;
      try { return JSON.parse(JSON.parse(o.text).content[0].text).find((c) => c.this_computer); } catch { return null; }
    }, 'the computer this AI client runs on', 30000);
    h.check("the AI client's computers list marks the one it runs on", mine.name === computer, JSON.stringify(mine));
    h.check('a computer as new as the AI client lacks none of its tools', !('missing_tools' in mine), JSON.stringify(mine));

    const call = () => deskway(h, home, ['call', computer, 'computer_info']).done;
    let r = await call();
    h.check('before it is turned on, a call is refused as ai_off', r.code !== 0 && r.text.includes('ai_off'), r.text.slice(0, 120));

    // Turned on (asked first), the same call is answered by the computer.
    await a.eval(`${sw}.click()`);
    await a.until(`document.querySelector('.dialog h2')?.textContent.includes(${JSON.stringify(fixed('devices.aiOnTitle'))})`, 'the confirmation');
    await a.shot('web-ai-on-confirm', 1440, 900);
    await a.clickText('.dialog button', t('devices.allow'));
    await a.until(`${sw}?.getAttribute('aria-checked') === 'true'`, 'the switch on');
    h.check('the switch turns on once confirmed', true);
    await a.shot('web-devices-ai-on', 1440, 900);
    await a.shot('web-devices-ai-on-phone', 390, 844);
    await a.metrics(1440, 900);
    await sleep(500);
    r = await call();
    h.check('once on, the computer answers the AI client', r.code === 0 && r.text.includes(`"name": ${JSON.stringify(computer)}`), r.text.slice(0, 160));
    r = await deskway(h, home, ['call', computer, 'screenshot', '{"max_width": 640}']).done;
    h.check('a screenshot comes back as a picture of the display', r.code === 0 && r.text.includes('"mimeType": "image/jpeg"') && r.text.includes('"width": 640'), r.text.slice(0, 120));
    // The workspace: a command and its output, a program that runs on past its call, and a file
    // written, read back and deleted in this test's own folder.
    const tool = async (name, args) => {
      const o = await deskway(h, home, ['call', computer, name, JSON.stringify(args)]).done;
      try { return { ok: o.code === 0, v: JSON.parse(JSON.parse(o.text).content[0].text), text: o.text }; } catch { return { ok: false, v: {}, text: o.text }; }
    };
    const win = process.platform === 'win32';
    // Administrator rights: refused until a person allows them (asked first); then a run as an
    // administrator runs with them, or says there are none to use here (a host not elevated).
    const elevatedRun = { command: win ? 'Write-Output x' : 'echo x', elevated: true };
    let res = await tool('run', elevatedRun);
    h.check('elevated is refused until administrator rights are allowed', !res.ok && res.text.includes('elevation_off'), res.text.slice(0, 160));
    const adm = `document.querySelector('[data-ai-admin=${JSON.stringify(h.hostId)}]')`;
    await a.until(`!!${adm}`, 'the administrator rights switch, once AI is allowed');
    await a.eval(`${adm}.click()`);
    await a.until(`document.querySelector('.dialog h2')?.textContent.includes(${JSON.stringify(fixed('devices.adminOnTitle'))})`, 'asked first');
    await a.shot('web-admin-confirm', 1440, 900);
    await a.clickText('.dialog button', t('devices.allow'));
    await a.until(`${adm}?.getAttribute('aria-checked') === 'true'`, 'administrator rights on');
    await a.shot('web-devices-admin-on', 1440, 900);
    await sleep(500);
    res = await tool('run', elevatedRun);
    h.check('allowed, a run as an administrator runs, or says there are no administrator rights here', (res.ok && res.v.exit_code === 0) || res.text.includes('elevation_unavailable'), res.text.slice(0, 160));
    res = await tool('run', { command: win ? 'Write-Output deskway-e2e' : 'echo deskway-e2e' });
    h.check('run answers with what the command printed', res.ok && res.v.exit_code === 0 && res.v.output.includes('deskway-e2e'), res.text.slice(0, 160));
    res = await tool('run', { command: win ? 'Start-Sleep 600' : 'sleep 600', wait_ms: 200 });
    const slow = res.v.id;
    h.check('a program that takes longer runs on, with an id', res.ok && res.v.running === true && Number.isInteger(slow), res.text.slice(0, 160));
    res = await tool('proc_list', {});
    h.check('proc_list shows it running', res.ok && res.v.some((p) => p.id === slow && p.running), res.text.slice(0, 160));
    res = await tool('proc_kill', { id: slow });
    h.check('proc_kill ends it', res.ok && res.v.running === false, res.text.slice(0, 160));
    // A shell in a terminal: typed into, read back without its control sequences, and left.
    res = await tool('proc_start', {});
    const shell = res.v.id;
    h.check('proc_start starts a shell in a terminal', res.ok && res.v.running === true && Number.isInteger(shell), res.text.slice(0, 160));
    res = await tool('proc_write', { id: shell, text: win ? "Write-Output ('deskway' + '-pty')" : "echo deskway-pty", keys: ['enter'] });
    let seen = '';
    for (let at = res.v.next, i = 0; res.ok && !seen.includes('deskway-pty') && i < 10; i++) {
      const r2 = await tool('proc_read', { id: shell, from: at, wait_ms: 2000 });
      seen += r2.v.output || '';
      at = r2.v.next;
    }
    h.check('proc_write types into it and proc_read reads its answer, plain', seen.includes('deskway-pty') && !seen.includes(''), JSON.stringify(seen.slice(-160)));
    res = await tool('proc_kill', { id: shell });
    h.check('a terminal program ends like any other', res.ok && res.v.running === false, res.text.slice(0, 160));
    const file = join(home, 'e2e.txt');
    res = await tool('fs_write', { path: file, text: '你好, deskway' });
    res = res.ok && await tool('fs_read', { path: file });
    h.check('a file written is read back', res.ok && res.v.text === '你好, deskway', res.text?.slice(0, 160));
    // A file of 1.5 MB, not text, there and back: in pieces, the same bytes at both ends.
    const bytes = Buffer.from(Array.from({ length: 1572864 + 7 }, (_, i) => (i * 31) % 251));
    const [src, there, back] = ['copy-src.bin', 'copy-there.bin', 'copy-back.bin'].map((n) => join(home, n));
    writeFileSync(src, bytes);
    res = await tool('copy_to', { from: src, to: there });
    h.check('copy_to copies a file there, checked by SHA-256', res.ok && res.v.bytes === bytes.length && /^[0-9a-f]{64}$/.test(res.v.sha256), res.text.slice(0, 200));
    res = await tool('copy_from', { from: there, to: back });
    h.check('copy_from copies it back, the same bytes', res.ok && readFileSync(back).equals(bytes), res.text.slice(0, 200));
    res = await tool('fs_delete', { path: file });
    res = await tool('fs_stat', { path: file });
    h.check('a file deleted is gone', !res.ok && res.text.includes('not_found'), res.text.slice(0, 160));
    // A tunnel to a web server on the computer's loopback, for as long as the AI client that
    // asked stays open: 3 MB through it, the same bytes; a port nothing listens on is refused.
    const body = Buffer.from(Array.from({ length: 3 << 20 }, (_, i) => (i * 7) % 256));
    const web = createServer((_, res) => res.end(body));
    await new Promise((res) => web.listen(0, '127.0.0.1', res));
    const tunneller = mcpOpen(h, home);
    const tr = await tunneller.call('tunnel', { computer, port: web.address().port });
    const tv = (() => { try { return JSON.parse(tr.result.content[0].text); } catch { return {}; } })();
    h.check("tunnel answers with a local port for the computer's", !tr.result?.isError && Number.isInteger(tv.local_port), JSON.stringify(tr).slice(0, 160));
    const got = Number.isInteger(tv.local_port) ? Buffer.from(await (await fetch(`http://127.0.0.1:${tv.local_port}/`)).arrayBuffer()) : Buffer.alloc(0);
    h.check('a connection through it reaches the program there, all its bytes', got.equals(body), `${got.length} of ${body.length} bytes`);
    await tunneller.close();
    web.close();
    res = await tool('tunnel', { port: 1 });
    h.check('a port nothing listens on is refused', !res.ok && res.text.includes('not_found'), res.text.slice(0, 160));
    // Waiting on the screen, and what is open and installed (read only: nothing is touched).
    res = await tool('wait_for', { until: 'change', timeout_ms: 500 });
    h.check('wait_for says whether the display changed in time', res.ok && typeof res.v.met === 'boolean' && res.v.until === 'change' && res.v.waited_ms >= 0, res.text.slice(0, 160));
    res = await tool('windows', {});
    h.check('windows lists the windows open, in screen coordinates', res.ok && Array.isArray(res.v) && res.v.every((w) => Number.isInteger(w.id) && typeof w.title === 'string' && Number.isInteger(w.x) && w.pid > 0), res.text.slice(0, 160));
    const open = res.ok && res.v[0];
    if (open) {
      res = await tool('ui_tree', { window: open.id, depth: 3, max_nodes: 50 });
      h.check("ui_tree reads a window's controls", res.ok && typeof res.v.root.role === 'string' && typeof res.v.root.id === 'string' && typeof res.v.truncated === 'boolean', res.text.slice(0, 160));
    }
    res = await tool('app_list', {});
    h.check('app_list names the apps the person can open', res.ok && res.v.length > 0 && res.v.every((a) => a.name && a.path), res.text.slice(0, 160));
    // The test computer is view-only: input and what changes the screen are refused before they
    // could reach this machine.
    for (const [name, args] of [['click', { x: 10, y: 10 }], ['app_open', { name: 'notepad' }], ['window_act', { id: 1, action: 'minimize' }], ['app_quit', { name: 'notepad' }], ['ui_act', { window: 1, element: '1', action: 'invoke' }]]) {
      r = await deskway(h, home, ['call', computer, name, JSON.stringify(args)]).done;
      h.check(`a view-only computer refuses the AI client's ${name}`, r.code !== 0 && r.text.includes('view_only'), r.text.slice(0, 120));
    }

    // The computer's record of what AI clients did, read back: this client's runs, newest first,
    // a page at a time.
    res = await tool('audit_read', { tool: 'run', limit: 2 });
    const recs = res.ok ? res.v.calls : [];
    h.check('audit_read reads back the calls made here, newest first, a page at a time', recs.length === 2 && res.v.total > 2 && recs.every((x) => x.tool === 'run' && x.client === 'Claude E2E' && x.result) && recs[0].at >= recs[1].at && res.v.tools.includes('screenshot'), res.text.slice(0, 200));
    // ...and on the computer's page, below its screens, narrowed by tool.
    await a.v(`d.show('detail', ${JSON.stringify(h.hostId)})`);
    const rows = `[...document.querySelectorAll('.audit .audit-call')]`;
    // Closed on entering the page: only its title, nothing asked of the computer.
    await a.until(`!!document.querySelector('.audit.closed .audit-toggle[aria-expanded="false"]')`, "the AI activity closed on the computer's page");
    h.check("entering the computer's page leaves the AI activity closed, its calls unlisted", await a.eval(`${rows}.length === 0 && !document.querySelector('.audit-filters')`));
    await a.eval(`document.querySelector('.audit-toggle').click()`);
    await a.until(`${rows}.length > 3`, "the AI activity on the computer's page", 20000);
    h.check("the computer's page lists the AI calls made there", true, await a.eval(`${rows}.length`));
    h.check("beside the title: AI control allowed, and the way to add an AI client", await a.eval(`(() => { const s = document.querySelector('.audit-ai'); return !!s && s.textContent.includes(${q('audit.allowed')}) && [...s.querySelectorAll('button')].some((b) => b.textContent.includes(${q('devices.addAI')})); })()`));
    await a.shot('ai-activity-wide', 1440, 900);
    await a.eval(`document.querySelector('.audit').scrollIntoView()`);
    await a.shot('ai-activity-records', 1440, 900);
    await a.eval(`window.scrollTo(0, document.body.scrollHeight)`);
    await a.shot('ai-activity-pager', 1440, 900);
    await a.eval(`document.querySelector('.audit').scrollIntoView()`);
    await a.shot('ai-activity-phone', 390, 844);
    await a.metrics(1440, 900);
    await a.eval(`window.scrollTo(0, 0)`);
    await a.eval(`(() => { const s = document.querySelector(${JSON.stringify(`.audit select[aria-label="${t('audit.tool')}"]`)}); s.value = 'run'; s.dispatchEvent(new Event('change')); })()`);
    await a.until(`${rows}.length > 0 && ${rows}.every((r) => r.querySelector('.col-tool').textContent === 'run')`, 'only run calls', 20000);
    h.check('choosing a tool shows only its calls', true);
    // A call opens in place to what was recorded of it: its whole arguments, never content.
    await a.eval(`document.querySelector('.audit .audit-call .audit-line').click()`);
    await a.until(`!!document.querySelector('.audit .audit-call.open .audit-detail')`, 'a call opened');
    h.check('a call opens in place to its details', await a.eval(`document.querySelector('.audit-call.open .audit-line').getAttribute('aria-expanded') === 'true' && document.querySelector('.audit-detail').textContent.includes('run')`));
    await a.eval(`document.querySelector('.audit').scrollIntoView()`);
    await a.shot('ai-activity-open', 1440, 900);
    await a.eval(`document.querySelector('.audit').scrollIntoView()`);
    await a.shot('ai-activity-open-phone', 390, 844);
    await a.metrics(1440, 900);
    // Its title, pressed again, closes it.
    await a.eval(`document.querySelector('.audit-toggle').click()`);
    h.check('pressing the title again closes the AI activity', await a.eval(`!!document.querySelector('.audit.closed') && ${rows}.length === 0`));
    await a.v(`d.show('devices')`);
    // An AI client at work holds the computer: its MCP server stays open.
    const held = mcpOpen(h, home);
    const info = await held.call('computer_info', { computer });
    h.check('an AI client kept open holds the computer it called', !info.result?.isError, JSON.stringify(info).slice(0, 160));
    // The page says the AI client is controlling it, beside nobody.
    await a.v(`d.show('list')`);
    await a.until(`document.body.textContent.includes(${q('status.aiUsing', { name: 'Claude E2E' })})`, 'the AI client in the list');
    h.check('the computers list says the AI client is controlling it', true);
    await a.shot('web-list-ai', 1440, 900);
    await a.shot('web-list-ai-phone', 390, 844);
    await a.metrics(1440, 900);
    // A person's session beside the AI client: its toolbar says the AI client controls it too.
    await a.v(`d.open(${JSON.stringify(h.hostId)})`);
    await a.until(`!!s && s.info().state === 'active'`, 'a session beside the AI client', 30000);
    await a.until(`!document.querySelector('.toolbar .tool-ai').hidden && document.querySelector('.toolbar .tool-ai').textContent.includes(${q('tb.aiAlso', { name: 'Claude E2E' })})`, 'the AI client in the session');
    h.check("a person's session beside the AI client says it controls the computer too", true);
    await a.v(`s.showToolbar && s.showToolbar()`);
    await a.shot('web-session-ai', 1440, 900);
    await a.v(`s.leave()`);
    await a.until(`!s`, 'the session left');
    // The AI client closes (it quit, was closed or crashed): the computer is given up at once,
    // not after 10 idle minutes.
    await held.close();
    await a.v(`d.show('list')`);
    await a.until(`!document.body.textContent.includes(${q('status.aiUsing', { name: 'Claude E2E' })})`, 'the AI client no longer named once it closed', 15000);
    h.check('an AI client that closes gives the computer up at once', true);

    // Off again: the AI client's session ends and the next call is refused.
    await a.v(`d.show('devices')`);
    await a.until(`${sw}?.getAttribute('aria-checked') === 'true'`, 'the switch shown on');
    // The list may be drawn again under the press (the computer was just given up), which then
    // reaches a switch no longer shown: pressed once more if nothing came of it.
    for (let tries = 1; ; tries++) {
      await a.eval(`${sw}.click()`);
      const off = await a.until(`${sw}?.getAttribute('aria-checked') === 'false'`, 'the switch off', 3000).then(() => true, () => false);
      if (off) break;
      if (tries === 3) throw new Error(`the switch did not turn off: ${await a.eval(`document.querySelector('.dialog')?.textContent || 'no dialog'`)}`);
    }
    await sleep(500);
    r = await call();
    h.check('turned off, the next call is refused as ai_off', r.code !== 0 && r.text.includes('ai_off'), r.text.slice(0, 120));
    // Signing out stops the proxy this test started.
    await deskway(h, home, ['logout', '--forget']).done;
  },
};
