
// The foreground Windows workspace, with an isolated server, controller and host profile.
import { execFile, spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { mkdirSync, readdirSync, readFileSync, realpathSync, statSync, writeFileSync } from 'node:fs';
import { join, parse, resolve, sep } from 'node:path';
import { promisify } from 'node:util';
import { keyboardOf, lit, q, issueAdminLink, redeemSetup, sleep, t } from './harness.mjs';
import { TerminalChannel } from '../../web/static/js/session/terminal-channel.js';

const command = promisify(execFile);
const user = 'workspace-trial';
const password = 'workspace trial password';
const buttons = '.computer-actions button';
const terminalButton = '[data-session-view="terminal"]';
const title = 'document.querySelector(".dialog h2")?.textContent';
const terminal = '[...document.querySelectorAll(".xterm-rows > div")].map(row => row.textContent).join("\\n")';
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');

async function waitFor(read, label) {
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    const result = await read();
    if (result) return result;
    await sleep(100);
  }
  throw new Error('Timed out waiting for ' + label);
}

function start(exe, args, env) {
  const child = spawn(exe, args, { env, windowsHide: true, stdio: ['ignore', 'ignore', 'pipe'] });
  let log = '';
  child.stderr.setEncoding('utf8').on('data', (text) => { log += text; });
  child.on('error', (error) => { log += error.message; });
  const closed = new Promise((done) => child.once('close', done));
  return {
    get log() { return log; },
    async stop() { child.kill(); await closed; },
  };
}

async function click(page, selector, label) {
  const found = await page.clickText(selector, label, { exact: true });
  if (!found) throw new Error('Missing button ' + label);
}

async function press(page, code, key, keyCode) {
  const input = keyboardOf(page);
  await input.press(code, true, { key, windowsVirtualKeyCode: keyCode });
  await input.press(code, false, { key, windowsVirtualKeyCode: keyCode });
}

async function shell(page, line) {
  await page.eval(`(() => {
    const stage = document.querySelector('.terminal-stage');
    stage.focus();
    const clipboardData = new DataTransfer();
    clipboardData.setData('text/plain', ${lit(line)});
    stage.dispatchEvent(new ClipboardEvent('paste', { clipboardData, bubbles: true }));
  })()`);
  await press(page, 'Enter', 'Enter', 13);
}

async function enterWorkspace(page, label) {
  await click(page, buttons, t(label));
  if (await page.eval(`${title} === ${q('here.confirmTitle')}`)) {
    await click(page, '.dialog button', t('here.confirmGo'));
  }
}

async function leave(page, close = false) {
  await click(page, '.toolbar button', t('tb.disconnect'));
  await click(page, '.dialog button', t(close ? 'terminal.closeAndDisconnect' : 'tb.disconnect'));
  await page.until('!document.querySelector(".session")', 'the session closes');
}

async function openFolder(page, name) {
  // A large folder (the system's temporary one) arrives in pages: scrolling to the end asks for the next.
  await page.until(`(document.querySelector('.files-list').scrollTop = 1e9, [...document.querySelectorAll('.files-name')].some(e => e.textContent === ${lit(name)}))`, name, 120000);
  await click(page, 'button.files-name', name);
  await page.until(`document.querySelector('.files-path [aria-current]')?.textContent === ${lit(name)}`, name);
  await page.until('!document.querySelector(".files-surface .notice").textContent.includes("Loading")', 'the listing');
}

const rowMenu = async (page, name, action) => {
  const label = t('files.entryActions', { name });
  await page.until(`!!document.querySelector('.files-row button[aria-label=' + CSS.escape(${lit(label)}) + ']')`, name);
  await page.eval(`[...document.querySelectorAll('.files-row button')].find(e => e.getAttribute('aria-label') === ${lit(label)}).click()`);
  await click(page, '.menu:not([hidden]) .menu-item', t(action));
};

async function nameField(page, text) {
  await page.eval(`document.querySelector('.dialog input').value = ${lit(text)}`);
  await click(page, '.dialog button', t('common.save'));
}

async function upload(page, path) {
  await click(page, '.files-bar button', t('files.upload'));
  const { result: { root } } = await page.send('DOM.getDocument');
  const { result: { nodeId } } = await page.send('DOM.querySelector', { nodeId: root.nodeId, selector: '.files-bar input[type=file]' });
  await page.send('DOM.setFileInputFiles', { nodeId, files: [path] });
}

export const area = {
  name: 'windows-workspace',
  needs: [],
  async run(h) {
    const address = new URL(h.opts.url);
    if (process.platform !== 'win32' || address.hostname !== 'localhost' || address.protocol !== 'http:' || !address.port
        || !h.opts.serverExe || !h.opts.hostExe) {
      throw new Error('windows-workspace needs Windows, --url http://localhost:PORT, --server-exe and --host-exe');
    }
    const hostExe = resolve(h.opts.hostExe), serverExe = resolve(h.opts.serverExe);
    const dir = join(h.profile, 'workspace');
    const home = join(dir, 'host'), remote = join(dir, 'remote'), local = join(dir, 'local'), downloads = join(dir, 'downloads');
    for (const folder of [home, remote, local, downloads]) mkdirSync(folder, { recursive: true });
    const state = join(dir, 'state.json'), lock = join(dir, 'lock');
    writeFileSync(lock, 'ready');
    const appData = join(home, 'AppData'), roaming = join(appData, 'Roaming'), localData = join(appData, 'Local');
    for (const folder of [roaming, localData]) mkdirSync(folder, { recursive: true });
    const env = {
      ...process.env, APPDATA: roaming, LOCALAPPDATA: localData, USERPROFILE: home, HOME: home,
      DESKWAY_HOME: home, DESKWAY_TEST_WORKSPACE_LOCK: lock,
      // PSReadLine finds the Windows roaming folder even with APPDATA set.
      DESKWAY_TEST_WORKSPACE_SHELL: "powershell.exe -NoLogo -NoProfile -NoExit -Command Set-PSReadLineOption -HistorySavePath (Join-Path $env:APPDATA 'workspace-history.txt')",
    };
    const exec = (exe, args, env) => command(exe, args, { env, windowsHide: true, timeout: 30000, encoding: 'utf8' });
    const server = start(serverExe, ['-listen', '127.0.0.1:' + address.port, '-data', state]);
    let host, panel, localChannel;
    try {
      await waitFor(async () => server.log.includes('listening on') && (await fetch(h.opts.url).catch(() => null))?.ok, 'server');
      const page = await h.newPage();
      await page.addAuthenticator();
      await page.eval('localStorage.setItem("deskway.autofull", "0")');
      await redeemSetup(page, await issueAdminLink(serverExe, state, h.opts.url), { user, password });

      const computer = 'Windows workspace';
      await exec(hostExe, ['enroll', '--server', h.opts.url, '--user', user, '--name', computer],
        { ...env, DESKWAY_PASSWORD: password });
      const { id } = JSON.parse(readFileSync(join(home, 'host.json'), 'utf8'));
      const device = `d.devices().find(x => x.id === ${lit(id)})`;
      host = start(hostExe, ['net', '--dev'], env);
      await page.until(`${title} === ${q('approve.computerTitle')}`, 'host approval');
      const approval = await waitFor(() => {
        if (!panel || panel.error) { panel?.close(); panel = h.panelPipe(home); }
        return panel.statuses.find((status) => status.waiting && status.approval_code)?.approval_code;
      }, 'local approval code');
      h.check('approval names the isolated foreground host', await page.eval(
        `document.querySelector('.dialog').textContent.replace(/\\D/g, '').includes(${lit(approval.replace(/\D/g, ''))})`));
      await click(page, '.dialog button', t('common.approve'));
      await page.until(`(${device})?.online`, 'the host online');
      await click(page, '.card-open', computer);

      await enterWorkspace(page, 'terminal.label');
      await page.until(`(${terminal}).includes('PS ')`, 'PowerShell prompt', 15000);
      await shell(page, "if ((Get-PSReadLineOption).HistorySavePath.StartsWith($env:APPDATA + '\\', [StringComparison]::OrdinalIgnoreCase)) { Write-Output ('history-' + 'isolated') }");
      await page.until(`(${terminal}).includes('history-isolated')`, 'isolated PowerShell history');
      await shell(page, "Write-Output ('deskway-' + 'ready')");
      await page.until(`(${terminal}).includes('deskway-ready')`, 'command output');
      h.check('PowerShell opens and returns output', true);

      await shell(page, "'size=' + $Host.UI.RawUI.WindowSize.Width");
      const width = await waitFor(() => page.eval(`(${terminal}).match(/size=(\\d+)/)?.[1]`), 'initial shell width');
      await page.metrics(860, 720);
      await shell(page, "'changed-size=' + $Host.UI.RawUI.WindowSize.Width");
      await page.until(`/changed-size=(\\d+)/.test(${terminal}) && (${terminal}).match(/changed-size=(\\d+)/)[1] !== ${lit(width)}`, 'resized shell');
      h.check('resizing changes the ConPTY width', true);

      await shell(page, "Write-Output ('sleep-' + 'started'); Start-Sleep -Seconds 60");
      await page.until(`(${terminal}).includes('sleep-started')`, 'running command');
      const keyboard = keyboardOf(page);
      await keyboard.press('ControlLeft', true);
      await keyboard.press('KeyC', true);
      await keyboard.press('KeyC', false);
      await keyboard.press('ControlLeft', false);
      await page.until(`(${terminal}).split('sleep-started').slice(1).join('').includes('PS ')`, 'PowerShell returns to the prompt after Ctrl-C', 8000);
      await shell(page, "Write-Output ('interrupt-' + 'returned')");
      await page.until(`(${terminal}).includes('interrupt-returned')`, 'new command after interrupt', 8000);
      h.check('Ctrl-C interrupts the running command', true);

      const originalId = await waitFor(() => page.v(`(${device})?.terminals?.find(x => x.attached)?.id`), 'original terminal ID');
      await leave(page);
      await page.until(`(${device})?.terminals?.some(x => x.id === ${originalId} && !x.attached)`, 'the detached terminal');
      h.check('the computer page offers Terminal and Files', await page.eval(
        `!document.querySelector('.terminal-kept button') &&
        [...document.querySelectorAll('${buttons}')].map(e => e.textContent).join('|') === ${lit([t('terminal.label'), t('files.label')].join('|'))}`));
      await enterWorkspace(page, 'terminal.label');
      await page.until(`(${terminal}).includes('interrupt-returned')`, 'replayed terminal output');
      h.check('leave keeps the terminal and Terminal replays it', true);

      await click(page, terminalButton, t('terminal.label'));
      h.check('the terminal panel marks the current terminal and ends with New terminal', await page.eval(
        `document.querySelector('.terminal-picker .notice button:disabled')?.textContent === ${q('terminal.current')} &&
        document.querySelector('.terminal-picker > button:last-child')?.textContent === ${q('terminal.new')}`));
      await click(page, '.terminal-picker > button', t('terminal.new'));
      const secondId = await waitFor(() => page.v(
        `(${device})?.terminals?.find(x => x.attached && x.id !== ${originalId})?.id`), 'new terminal ID');
      await page.until(`document.querySelector('${terminalButton}').textContent === ${q('terminal.numbered', { id: secondId })} &&
        (${terminal}).includes('PS ')`, 'new terminal prompt');
      await shell(page, "Write-Output ('second-' + 'terminal')");
      await page.until(`(${terminal}).includes('second-terminal')`, 'second terminal output');
      await click(page, terminalButton, t('terminal.numbered', { id: secondId }));
      h.check('the numbered Terminal button expands the panel', await page.eval(
        `document.querySelector('${terminalButton}').getAttribute('aria-expanded') === 'true' &&
        !document.querySelector('${terminalButton} .route-arrow').hidden`));
      await click(page, '.terminal-picker .notice button', t('terminal.return'));
      await page.until(`document.querySelector('${terminalButton}').textContent === ${q('terminal.numbered', { id: originalId })} &&
        (${terminal}).includes('interrupt-returned')`, 'return to the original terminal');

      await click(page, '[data-session-view]', t('files.label'));
      await page.until('!!document.querySelector(".files-path [aria-current]")', 'Files opens');
      await click(page, terminalButton, t('terminal.numbered', { id: originalId }));
      h.check('from Files the Terminal button returns to the terminal view', await page.eval(
        `!document.querySelector('.terminal-stage').hidden && !document.querySelector('.terminal-picker')`));
      await click(page, '[data-session-view]', t('files.label'));
      h.check('Files opens in the user profile', await page.eval(
        `[...document.querySelectorAll('.files-path button')].slice(1).map(e => e.textContent).join('\\\\').toLowerCase() === ${lit(realpathSync(home).toLowerCase())}`));
      await click(page, '.files-path button', '/');
      await page.until(`[...document.querySelectorAll('.files-name')].some(e => /^[A-Z]:$/.test(e.textContent))`, 'drive roots');
      h.check('the root lists drives and has no upload or drive mutation', await page.eval(
        `[...document.querySelectorAll('.files-bar button')].find(e => e.textContent === ${q('files.upload')}).disabled && !document.querySelector('.files-row .more')`));
      const drive = parse(remote).root;
      for (const name of [drive.slice(0, -1), ...remote.slice(drive.length).split(sep)]) await openFolder(page, name);

      await page.eval('document.querySelector(".files-bar .more").click()');
      await click(page, '.menu:not([hidden]) .menu-item', t('files.mkdir'));
      await nameField(page, 'NUL.txt');
      await page.until(`document.querySelector('.dialog').textContent.includes(${q('files.error.invalid_name')})`, 'reserved name refusal');
      await nameField(page, 'folder');
      await page.until('!document.querySelector(".dialog")', 'new folder');
      await rowMenu(page, 'folder', 'common.rename');
      await nameField(page, 'renamed');
      await page.until('!document.querySelector(".dialog")', 'rename');
      h.check('new folder, reserved name refusal and rename', readdirSync(remote).includes('renamed'));
      await rowMenu(page, 'renamed', 'files.delete');
      await click(page, '.dialog button', t('files.delete'));
      await waitFor(() => !readdirSync(remote).includes('renamed'), 'delete');
      h.check('delete removes the renamed folder', true);

      const source = join(local, 'data.bin');
      const data = randomBytes(4 * 1024 * 1024);
      writeFileSync(source, data);
      const target = join(remote, 'data.bin');
      const verified = async (path, expected) => waitFor(() => {
        try { return sha256(readFileSync(path)) === sha256(expected); } catch { return false; }
      }, 'verified ' + path);
      await upload(page, source);
      await verified(target, data);
      await page.eval('Object.defineProperty(window,"showSaveFilePicker",{value:undefined,configurable:true})');
      await (await h.browserPage()).send('Browser.setDownloadBehavior', { behavior: 'allow', downloadPath: downloads });
      await rowMenu(page, 'data.bin', 'files.download');
      await page.until(`document.querySelector('.file-transfer-strip')?.textContent.includes(${lit(t('common.save'))})`, 'download verified');
      await click(page, '.file-transfer-strip button', t('common.save'));
      await verified(join(downloads, 'data.bin'), data);
      h.check('upload and download match SHA-256', true);

      writeFileSync(source, 'replaced');
      await upload(page, source);
      await page.until(`${title} === ${q('files.transfer.conflictTitle', { name: 'data.bin' })}`, 'upload collision');
      await click(page, '.dialog button', t('files.transfer.replace'));
      await verified(target, 'replaced');
      writeFileSync(source, 'skipped');
      await upload(page, source);
      await page.until(`${title} === ${q('files.transfer.conflictTitle', { name: 'data.bin' })}`, 'second collision');
      await click(page, '.dialog button', t('files.transfer.skip'));
      await page.until(`!document.querySelector('.dialog')`, 'Skip closes the dialog');
      h.check('Replace publishes the new bytes and Skip preserves them', readFileSync(target, 'utf8') === 'replaced');

      const resume = join(local, 'resume.bin');
      writeFileSync(resume, data);
      await page.eval(`(() => {
        const original = File.prototype.slice;
        window.delayResume = true;
        File.prototype.slice = function(start, ...args) {
          const part = original.call(this, start, ...args);
          if (this.name === 'resume.bin' && start > 0 && window.delayResume) {
            const read = part.arrayBuffer.bind(part);
            part.arrayBuffer = async () => {
              while (window.delayResume) await new Promise(resolve => setTimeout(resolve, 50));
              return read();
            };
          }
          return part;
        };
        const create = RTCPeerConnection.prototype.createDataChannel;
        RTCPeerConnection.prototype.createDataChannel = function(label, ...args) {
          const channel = create.call(this, label, ...args);
          if (label === 'file-transfer') channel.addEventListener('message', ({data}) => {
            if (typeof data === 'string') {
              const m = JSON.parse(data);
              if (m.t === 'ready') window.resumeOffset = m.offset;
            }
          });
          return channel;
        };
      })()`);
      await upload(page, resume);
      await waitFor(() => readdirSync(remote).some((name) => name.endsWith('.partial')
        && statSync(join(remote, name)).size > 0), 'partially staged upload');
      writeFileSync(lock, 'locked');
      await page.until(`(${device})?.workspace_access === 'locked' && !document.querySelector('.session')`, 'lock ends the data session');
      await page.until(`document.querySelector('.dialog')?.textContent.includes(${q('detail.workspaceLocked')})`, 'session lock reason');
      h.check('lock disables Terminal and Files while still offering the desktop', await page.eval(
        `document.querySelectorAll('.computer-actions button:disabled').length === 2 &&
        !document.querySelector('.terminal-kept button') &&
        [...document.querySelectorAll('.dialog button')].some(e => e.textContent === ${q('detail.hint')})`));
      await click(page, '.dialog button', t('common.back'));
      writeFileSync(lock, 'ready');
      await page.until(`!(${device})?.workspace_access`, 'unlock');
      await page.eval('window.delayResume = false');
      await enterWorkspace(page, 'files.label');
      await verified(join(remote, 'resume.bin'), data);
      h.check('a lock interrupts an upload and unlock resumes at its recorded offset', await page.eval('window.resumeOffset > 0'));
      await click(page, terminalButton, t('terminal.label'));
      await page.until(`(${terminal}).includes('interrupt-returned')`, 'kept terminal survived lock');
      await page.until(`document.querySelector('${terminalButton}').textContent === ${q('terminal.numbered', { id: originalId })}`, 'original terminal selected');
      await click(page, terminalButton, t('terminal.numbered', { id: originalId }));
      await click(page, '.terminal-picker .notice button', t('terminal.return'));
      await page.until(`(${terminal}).includes('second-terminal')`, 'second terminal survived lock');
      h.check('both terminals survive the lock', true);

      const openHere = (target, take) => {
        localChannel?.close();
        const channel = localChannel = h.terminalPipe(home);
        const outcomes = [];
        const stream = new TerminalChannel(channel, (_, consumed) => consumed(), (message) => outcomes.push(message));
        channel.onopen = () => stream.size(24, 80, target, take);
        return outcomes;
      };
      const localOutcomes = openHere();
      await waitFor(() => localOutcomes.some(m => m.t === 'opened' && m.id === originalId), 'local terminal returns to the free shell');
      await page.until(`(${device})?.terminals?.some(x => x.id === ${originalId} && x.attached && x.here)`, 'terminal attached at the computer');
      await click(page, terminalButton, t('terminal.numbered', { id: secondId }));
      await click(page, '.terminal-picker .notice button', t('terminal.takeOver'));
      await page.until(`${title} === ${q('terminal.takeOverTitle', { id: originalId })}`, 'browser takeover confirmation');
      await click(page, '.dialog button', t('terminal.takeOver'));
      await waitFor(() => localOutcomes.some(m => m.t === 'detached' && m.reason === 'taken'), 'local stream loses the attachment');
      await page.until(`(${terminal}).includes('interrupt-returned')`, 'browser takes the terminal and gets its replay');
      const takenBack = openHere(originalId, true);
      await waitFor(() => takenBack.some(m => m.t === 'opened' && m.id === originalId), 'local stream takes its terminal back');
      await page.until(`document.querySelector('.terminal-picker')?.textContent.includes(${q('terminal.takenHere', { id: originalId })}) && !!s && s.info().connection === 'connected'`, 'browser sees terminal takeover without losing its session');
      h.check('the local terminal and browser can take the same shell in both directions', true);
      const returnSecond = `${t('terminal.return')} · ${t('terminal.numbered', { id: secondId })} ·`;
      await click(page, `.terminal-picker .notice button[aria-label^=${lit(returnSecond)}]`, t('terminal.return'));
      await page.until(`(${terminal}).includes('second-terminal')`, 'browser returns to the second terminal');
      localChannel.close();
      await leave(page, true);

      await host.stop();
      await page.until(`!(${device})?.online`, 'host stopped');
      host = start(hostExe, ['net', '--dev', '--view-only'], env);
      await page.until(`(${device})?.workspace_access === 'view_only'`, 'view-only status');
      h.check('view-only host offers neither action', await page.eval(`!document.querySelector('.computer-actions') &&
        [...document.querySelectorAll('.notice')].some(e => e.textContent === ${q('audit.code.view_only')})`));
    } catch (error) {
      throw new Error([error.message, error.cause?.message, host?.log, server.log].filter(Boolean).join('\n'));
    } finally {
      localChannel?.close();
      panel?.close();
      await host?.stop();
      await server.stop();
    }
  },
};
