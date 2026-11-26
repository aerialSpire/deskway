// Run from this worktree on Windows with Docker Desktop in Linux-container mode:
// node target/trial/run.mjs --browser "C:\Program Files\Google\Chrome\Application\chrome.exe"
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Harness, centre, click, keyboardOf, lit, nameThisDevice, signIn, sleep, superIn, t, tap, touch } from '../../tests/e2e/harness.mjs';
import { FILE_TRANSFER_HEADER } from '../../web/static/js/proto.js';

const trial = fileURLToPath(new URL('.', import.meta.url));
const root = resolve(trial, '../..');
const transcript = join(trial, 'transcript.txt');
writeFileSync(transcript, '');
function record(text) {
  appendFileSync(transcript, text);
  process.stdout.write(text);
}

function start(exe, args, { env = process.env, input = '', quiet = false, timeout } = {}) {
  record(`> ${exe} ${args.map((arg) => JSON.stringify(arg)).join(' ')}\n`);
  const child = spawn(exe, args, { cwd: root, env, windowsHide: true, timeout });
  const result = { stdout: '', stderr: '', exited: false };
  for (const key of ['stdout', 'stderr']) {
    child[key].setEncoding('utf8').on('data', (text) => {
      result[key] += text;
      if (!quiet) record(text);
    });
  }
  child.stdin.on('error', () => {}); // A child can exit before reading its input.
  child.stdin.end(input);
  result.done = new Promise((resolve) => {
    child.once('error', (e) => { result.stderr += String(e); });
    child.once('close', (code) => {
      result.exited = true;
      resolve(code);
    });
  });
  return result;
}

function checked(exe, args, options) {
  return output(start(exe, args, options), exe);
}

async function waitFor(read, label, ms = 30000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const value = await read();
    if (value) return value;
    await sleep(200);
  }
  throw new Error(`timed out waiting for ${label}`);
}

async function freePort() {
  const server = createServer();
  await new Promise((resolve, reject) => server.once('error', reject).listen(0, '127.0.0.1', resolve));
  const { port } = server.address();
  await new Promise((resolve) => server.close(resolve));
  return port;
}

const rows = "[...document.querySelectorAll('.terminal-surface .xterm-rows > div')].map((r) => r.textContent.trim())";
const lastLine = `(${rows}).filter(Boolean).at(-1)`;
const keyboardButton = '.toolbar .tool-group button:not([data-session-view])';
const prompt = async (page, ms) => page.until(`${lastLine} === 'trial$'`, 'the shell prompt', ms);

// Real physical key events, including the keys a shell command needs for quoting.
const symbols = {
  ' ': ['Space', 32], "'": ['Quote', 222], '"': ['Quote', 222, true], '$': ['Digit4', 52, true],
  '!': ['Digit1', 49, true], '(': ['Digit9', 57, true], ')': ['Digit0', 48, true],
  '%': ['Digit5', 53, true], '&': ['Digit7', 55, true], '_': ['Minus', 189, true], '-': ['Minus', 189],
  '/': ['Slash', 191], '\\': ['Backslash', 220], ';': ['Semicolon', 186],
  '=': ['Equal', 187], '+': ['Equal', 187, true], '>': ['Period', 190, true], '.': ['Period', 190],
  '|': ['Backslash', 220, true], '[': ['BracketLeft', 219], ']': ['BracketRight', 221], '<': ['Comma', 188, true], ',': ['Comma', 188],
  ':': ['Semicolon', 186, true], '*': ['Digit8', 56, true], '?': ['Slash', 191, true], '{': ['BracketLeft', 219, true], '}': ['BracketRight', 221, true], '#': ['Digit3', 51, true], '@': ['Digit2', 50, true], '^': ['Digit6', 54, true], '~': ['Backquote', 192, true], '`': ['Backquote', 192],
};
async function command(page, text) {
  await page.eval("document.querySelector('.stage').focus({ preventScroll: true })");
  const { press } = keyboardOf(page);
  for (const key of [...text, 'Enter']) {
    assert(key === 'Enter' || symbols[key] || /^[a-z0-9]$/i.test(key), `trial has no physical key for ${key}`);
    const [code, windowsVirtualKeyCode, shifted] = symbols[key] || (key === 'Enter' ? ['Enter', 13] : [
      (/^[0-9]$/.test(key) ? 'Digit' : 'Key') + key.toUpperCase(), key.toUpperCase().charCodeAt(0), /^[A-Z]$/.test(key),
    ]);
    if (shifted) await press('ShiftLeft', true);
    await press(code, true, { key, windowsVirtualKeyCode });
    await press(code, false, { key, windowsVirtualKeyCode });
    if (shifted) await press('ShiftLeft', false);
  }
}

async function interrupt(page) {
  const { press } = keyboardOf(page);
  await press('ControlLeft', true);
  await press('KeyC', true);
  await press('KeyC', false);
  await press('ControlLeft', false);
}

async function paste(page, text) {
  await page.eval(`(() => {
    const clipboardData = new DataTransfer();
    clipboardData.setData('text/plain', ${lit(text)});
    document.querySelector('.terminal-surface').dispatchEvent(new ClipboardEvent('paste', { clipboardData, bubbles: true, cancelable: true }));
  })()`);
}

async function newBrowser(harness, url, shared = false) {
  const page = shared ? await harness.newPage() : await harness.contextPage();
  // Both peers can reach the same real TURN server. Only its client port crosses Docker.
  const source = `{
    const header = ${lit(FILE_TRANSFER_HEADER)};
    window.trialPeers = [];
    window.trialFileTraffic = [];
    const count = (traffic, data) => {
      if (!(data instanceof ArrayBuffer)) return;
      const frame = new DataView(data);
      const stats = traffic[frame.getUint32(0)] ??= { offset: Number(frame.getBigUint64(4)), bytes: 0 };
      stats.bytes += data.byteLength - header;
    };
    const Peer = RTCPeerConnection;
    window.RTCPeerConnection = class extends Peer {
      constructor(settings) {
        const iceServers = settings.iceServers.map((server) => ({
          ...server, urls: server.urls.filter((url) => url.startsWith('turn:') && url.endsWith('?transport=tcp')),
        })).filter((server) => server.urls.length);
        super({ ...settings, iceServers, iceTransportPolicy: 'relay' });
        window.trialPeers.push(this);
      }
      createDataChannel(label, options) {
        const channel = super.createDataChannel(label, options);
        if (label === 'control') window.trialControlChannel = channel;
        if (label === 'file-transfer') {
          window.trialFileChannel = channel;
          const traffic = { up: {}, down: {} };
          window.trialFileTraffic.push(traffic);
          channel.addEventListener('message', ({ data }) => count(traffic.down, data));
          channel.send = (data) => {
            RTCDataChannel.prototype.send.call(channel, data);
            count(traffic.up, data);
          };
        }
        return channel;
      }
    };
    window.trialStyleViolations = [];
    addEventListener('securitypolicyviolation', (e) => {
      if (e.effectiveDirective.startsWith('style-src')) window.trialStyleViolations.push(e.effectiveDirective);
    });
  }`;
  if (shared) await page.eval(source);
  else {
    await page.send('Page.addScriptToEvaluateOnNewDocument', { source });
    await page.send('Page.navigate', { url });
  }
  await page.send('Page.bringToFront');
  return page;
}

const terminalEntry = '[data-page="detail"] .computer-actions .btn, [data-page="detail"] .terminal-kept .btn';
const keptNotice = '[data-page="detail"] .terminal-kept';
async function detail(page, device) {
  await page.v(`d.hostCaps(null)`);
  await page.eval("localStorage.setItem('deskway.autofull', '0')");
  await page.v(`d.show('detail', ${lit(device)})`);
  await page.until(`!!document.querySelector(${lit(terminalEntry)})`, 'the terminal entry');
  assert(await page.eval(`document.querySelector('[data-page="detail"] .notice')?.textContent === ${lit(t('session.noDesktop'))}`));
}

async function openTerminal(page, device) {
  await detail(page, device);
  assert(await page.clickText(terminalEntry, t('terminal.label'), { exact: true })
    || await page.clickText(terminalEntry, t('terminal.return'), { exact: true }));
}

async function kept(page, name, ms) {
  const notice = `document.querySelector(${lit(keptNotice)})`;
  await page.until(`${notice}?.textContent.includes(${lit(name)}) && !!${notice}.querySelector('button.compact')`,
    'the kept terminal notice naming ' + name, ms);
}

async function returnTerminal(page) {
  assert(await page.clickText(keptNotice + ' button', t('terminal.return'), { exact: true }));
  await shellReady(page);
}

async function shellReady(page) {
  await page.until(`!!document.querySelector('.terminal-surface .xterm-rows') && !document.querySelector(${lit(keyboardButton)})?.disabled`,
    'the terminal opened', 35000);
}

async function clickToolbar(page, selector) {
  await page.eval("document.querySelector('.toolbar-tab').click()");
  let previous;
  const point = await waitFor(async () => {
    const rect = await page.eval(`(() => {
      const button = document.querySelector(${lit(selector)});
      if (!button || button.disabled) return null;
      const { left, top, right, bottom } = button.getBoundingClientRect();
      if (left < 0 || top < 0 || right > innerWidth || bottom > innerHeight || right <= left || bottom <= top) return null;
      return [left, top, right, bottom];
    })()`);
    const stable = rect && previous && rect.every((value, i) => value === previous[i]);
    previous = rect;
    if (stable) return [(rect[0] + rect[2]) / 2, (rect[1] + rect[3]) / 2];
  }, `the toolbar button ${selector} to stop moving`);
  await click(page, point);
}

async function disconnect(page, close = false) {
  await clickToolbar(page, '.toolbar button.end');
  assert(await page.clickText(close ? '.dialog button.danger' : '.dialog button.primary',
    t(close ? 'terminal.closeAndDisconnect' : 'tb.disconnect'), { exact: true }));
  await page.until("!!document.querySelector('[data-page=\"detail\"]')", 'back to the computer');
}


const user = 'linux-trial';
const hostUser = 'deskway-host';
const clientUser = 'deskway-client';
const computer = 'Linux trial';
const keeper = 'Terminal Keeper';
const clientName = 'Trial AI';
const asset = 'deskway-linux-x64';
// This network can use Debian's Rust image. Release builds for both architectures run in CI.
const image = 'debian:bookworm-slim';
const buildImage = 'rust:bookworm';
const platform = ['--platform', 'linux/amd64'];
const tag = 'deskway-trial-' + randomBytes(6).toString('hex');
const server = tag + '-server';
const host = tag + '-host';
const client = tag + '-client';
const runtime = tag + ':runtime';
const containers = [];
let folder, harness, runtimeBuilt, cleaning;

function container(name, args, options) {
  containers.push(name);
  return start('docker', ['run', '--rm', ...platform, '--name', name, ...args], options);
}

async function output(result, label) {
  assert.equal(await result.done, 0, label + ' failed or timed out: ' + result.stderr.trim());
  return result.stdout;
}

async function check(label, run) {
  try {
    const value = await run();
    record('PASS ' + label + '\n');
    return value;
  } catch (e) {
    record('FAIL ' + label + '\n');
    throw e;
  }
}

function cleanup() {
  cleaning ??= (async () => {
    try {
      if (harness) await harness.shutdown();
    } finally {
      for (const name of containers.reverse()) {
        const removal = start('docker', ['rm', '-f', name], { quiet: true, timeout: 15000 });
        if (await removal.done !== 0 && !removal.stderr.includes('No such container')) {
          record('FAIL removing ' + name + ': ' + removal.stderr.trim() + '\n');
          process.exitCode = 1;
        }
      }
      try {
        if (runtimeBuilt) await checked('docker', ['image', 'rm', runtime], { timeout: 30000 });
      } finally {
        if (folder) {
          assert.equal(dirname(resolve(folder)), resolve(trial));
          rmSync(folder, { recursive: true, force: true, maxRetries: 5, retryDelay: 250 });
        }
      }
    }
  })();
  return cleaning;
}

process.once('SIGINT', async () => {
  record('\nFAIL trial interrupted\n');
  try { await cleanup(); } finally { process.exit(130); }
});

async function run() {
  const browser = process.argv[process.argv.indexOf('--browser') + 1];
  assert(process.argv.includes('--browser') && browser, 'pass --browser with the path to Chrome or Edge');
  await check('Docker is running Linux containers', async () => {
    const docker = await checked('docker', ['info', '--format', '{{.OSType}}'], { timeout: 15000 });
    assert.equal(docker.trim(), 'linux', 'Docker must be running Linux containers');
  });
  folder = mkdtempSync(join(trial, 'run-'));
  // The e2e harness creates its disposable browser profile under the process temp directory.
  process.env.TEMP = folder;
  process.env.TMP = folder;
  const [http, turn, devtools] = await Promise.all([freePort(), freePort(), freePort()]);
  const url = `http://localhost:${http}`;
  const mount = 'type=bind,source=' + folder + ',target=/trial';
  const bind = ['--mount', mount];
  const joinServer = ['--network', 'container:' + server];
  const relay = ['-e', 'DESKWAY_TEST_RELAY=tcp'];
  const clientHome = '/home/' + clientUser;
  const hostHome = '/home/' + hostUser;
  const secret = randomBytes(24).toString('hex');
  const superPassword = randomBytes(24).toString('hex');

  await check('Linux CLI and host build with the release scripts', async () => {
    await output(container(tag + '-build', [
      ...bind, '--mount', 'type=bind,source=' + root + ',target=/src,readonly',
      '-w', '/src', '-e', 'CARGO_TARGET_DIR=/tmp/target', buildImage, 'sh', '-ec', [
        'apt-get update && apt-get install -y --no-install-recommends libssl-dev pkg-config',
        'sh scripts/build-linux-host.sh /trial',
        'sh scripts/build-ai-clients.sh /trial',
      ].join('\n'),
    ], { timeout: 1800000 }), 'Linux build');
  });
  await check('fresh Debian runtime image and trial server build', async () => {
    await checked('docker', ['build', ...platform, '--tag', runtime, '-'], {
      input: `FROM ${image}
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 procps iptables
`,
      timeout: 600000,
    });
    runtimeBuilt = true;
    await checked('go', ['build', '-o', join(folder, 'deskway-server'), './cmd/deskway-server'], {
      env: { ...process.env, GOOS: 'linux', GOARCH: 'amd64', CGO_ENABLED: '0' }, timeout: 600000,
    });
  });

  let page = await check('trial server and signed-in browser ready', async () => {
    const initial = (await output(container(tag + '-admin', [
      ...bind, image, 'sh', '-ec',
      'install /trial/deskway-server /tmp/deskway-server; exec /tmp/deskway-server admin "$1" -data /trial/state.json',
      'admin', user,
    ], { quiet: true, timeout: 300000 }), 'create the trial account')).match(/Initial password: (\S+)/)?.[1];
    assert(initial, 'admin did not return an initial password');

    // All Linux peers share the server's network namespace and can use the same loopback URL.
    const serving = container(server, [
      ...bind, '-p', `127.0.0.1:${http}:${http}/tcp`, '-p', `127.0.0.1:${turn}:${turn}/tcp`, image, 'sh', '-ec',
      'install /trial/deskway-server /tmp/deskway-server; exec /tmp/deskway-server "$@"', 'server',
      '-listen', `0.0.0.0:${http}`, '-data', '/trial/state.json', '-clients', '/trial', '-fetch-clients=false',
      '-public-ip', '127.0.0.1', '-turn-port', String(turn), '-turn-allow-private-for-tests',
    ]);
    await waitFor(async () => {
      assert(!serving.exited, 'the server exited before becoming ready');
      return (await fetch(url, { signal: AbortSignal.timeout(1000) }).catch(() => null))?.ok;
    }, 'the server');
    harness = new Harness({ browser, url, port: devtools, browserArgs: [] });
    await harness.launch();
    const page = await newBrowser(harness, url, true);
    await page.until("!!document.querySelector('form.login')", 'the login form');
    await signIn(page, { user, password: initial });
    await page.until("!!document.querySelector('form.choose')", 'the initial password form');
    await page.eval(`(() => {
      const inputs = document.querySelectorAll('form.choose input');
      ${lit([secret, secret, superPassword, superPassword])}.forEach((value, i) => { inputs[i].value = value; });
      document.querySelector('form.choose button[type="submit"]').click();
    })()`);
    await page.until('!!d?.me() && d.socketOpen()', 'the approved browser');
    await page.v("d.show('devices')");
    const ownMenu = `[...document.querySelectorAll('.device-row')].find((r) => r.textContent.includes(${lit(t('devices.tagThis'))}))?.querySelector('.device-actions .more')`;
    await page.until(`!!(${ownMenu})`, 'the first browser in My devices');
    await page.eval(`(${ownMenu}).click()`);
    assert(await page.clickText('.device-actions .menu-item', t('common.rename'), { exact: true }));
    await page.until("!!document.querySelector('.dialog.form input')", 'the browser name form');
    await page.eval(`(() => {
      document.querySelector('.dialog.form input').value = ${lit(keeper)};
      document.querySelector('.dialog.form button[type="submit"]').click();
    })()`);
    await page.until(`[...document.querySelectorAll('.device-row .name')].some((n) => n.textContent === ${lit(keeper)})`, 'the first browser renamed');
    return page;
  });

  const exec = (args, options) => checked('docker', ['exec', ...args], { timeout: 15000, ...options });
  const asHost = ['-i', host, 'runuser', '-u', hostUser, '--'];
  const asClient = ['--user', clientUser, '-e', 'HOME=' + clientHome, client];
  const inClient = (args, options) => exec([...asClient, ...args], options);
  const deskway = (args) => inClient(['deskway', ...args], { timeout: 120000 });

  const { device, foreground } = await check('browser approved the enrolled Linux host', async () => {
    await output(container(host, [
      '-d', '--init', '--cap-add', 'NET_ADMIN', ...joinServer, ...relay, '-e', 'LANG=C', '--mount', mount + ',readonly', runtime, 'sleep', 'infinity',
    ], { timeout: 15000 }), 'start the host container');
    await exec([host, 'sh', '-ec', [
      'useradd --create-home --shell /bin/sh "$1"',
      'tar -xzf /trial/deskway-host-linux-amd64.tar.gz -C /tmp',
      'install -m 755 /tmp/deskway-host-linux-amd64/deskway-host /usr/local/bin/deskway-host',
    ].join('\n'), 'install', hostUser]);
    const enrolled = await exec([...asHost, 'deskway-host', 'enroll', '--server', url, '--user', user, '--name', computer],
      { input: secret + '\n' });
    assert.match(enrolled, /Start the deskway-host service or run deskway-host/);
    const foreground = start('docker', ['exec', ...asHost, 'deskway-host']);
    const code = await waitFor(() => {
      assert(!foreground.exited, 'the host exited before approval');
      return foreground.stdout.match(/compare this code:\s*([\d ]+)/)?.[1].replace(/\D/g, '');
    }, 'Linux enrollment digits');
    assert.match(code, /^\d{6}$/);
    const approval = await waitFor(() => page.eval(`(async () => {
      const { state } = await import('/js/state.js');
      return state.approvals.find((a) => a.kind === 'device' && a.label === ${lit(computer)}) ?? null;
    })()`), 'the browser computer approval');
    assert.equal(approval.code.replace(/\D/g, ''), code);
    assert.equal(await page.eval(`(async () => {
      const { api } = await import('/js/net.js');
      return (await api('POST', '/api/approvals', ${lit({ kind: approval.kind, id: approval.id, allow: true })})).ok;
    })()`), true);
    await page.until(`d?.devices().some((x) => x.id === ${lit(approval.id)} && x.online && x.tools?.length)`,
      'the approved host online');
    return { device: approval.id, foreground };
  });

  await check('Add an AI client lists the Linux download and install command', async () => {
    await page.v("d.show('devices')");
    await page.until("!!document.querySelector('.device-add')", 'Add an AI client in My devices');
    await page.eval("document.querySelector('.device-add').click()");
    await page.until("!!document.querySelector('.ai-prompt-text')", 'the prompt');
    const prompt = await page.eval("document.querySelector('.ai-prompt-text').textContent");
    assert(prompt.includes('Linux x64') && prompt.includes(url + '/download/' + asset) && prompt.includes('chmod +x'));
    assert(await page.clickText('.ai-prompt-bar button', t('aiclient.copy'), { exact: true }));
    await page.until("!!document.querySelector('.ai-wait')", 'the AI sign-in window to open');
    await page.v("d.show('devices')");
  });

  await check('ordinary Linux user logs in after browser approval', async () => {
    await output(container(client, [
      '-d', ...joinServer, ...relay, '--mount', mount + ',readonly', runtime, 'sleep', 'infinity',
    ], { timeout: 15000 }), 'start the client container');
    await exec([client, 'sh', '-ec', 'useradd --create-home --shell /bin/sh "$1"; install -m 755 "/trial/$2" /usr/local/bin/deskway',
      'install', clientUser, asset]);
    assert.notEqual((await inClient(['id', '-u'])).trim(), '0');
    const login = start('docker', ['exec', ...asClient, 'deskway', 'login', '--server', url, '--user', user, '--name', clientName],
      { timeout: 90000 });
    const digits = await waitFor(() => {
      assert(!login.exited, 'deskway login exited before approval');
      return login.stderr.match(/numbers (\d{3}) (\d{3})/)?.slice(1).join('');
    }, 'the AI comparison code');
    await page.until(`document.querySelector('.dialog h2')?.textContent === ${lit(t('approve.aiTitle', { name: clientName }))} && !!document.querySelector('.dialog .code')`,
      'AI client approval');
    assert.equal(await page.eval("document.querySelector('.dialog .code').textContent.replace(/\\D/g, '')"), digits);
    assert(await page.clickText('.dialog button', 'Approve', { exact: true }));
    await output(login, 'deskway login');
    await page.until(`document.querySelector('.dialog h2')?.textContent === ${lit(t('approve.addedTitle'))}`, 'the signed AI endorsement');
    assert(await page.clickText('.dialog button', 'OK', { exact: true }));
    const entry = `d.devices().find((x) => x.id === ${lit(device)})`;
    const after = await page.v(`${entry}?.ai?.ts ?? 0`);
    await page.eval(`(async () => {
      const { setAi } = await import('/js/auth.js');
      await setAi(${lit(device)}, true, false, ${lit(after)});
    })()`);
    await page.until(`${entry}?.ai?.state === 'on'`, 'the host to accept signed AI authorization');
  });

  await check('deskway computers lists the online Linux host with AI allowed', async () => {
    const { structuredContent } = JSON.parse(await deskway(['computers']));
    const listed = structuredContent.result.find((c) => c.id === device);
    assert(listed, 'the host was not listed');
    assert.deepEqual([listed.name, listed.os, listed.online, listed.ai], [computer, 'linux', true, 'on']);
  });

  const call = async (tool, args) => JSON.parse(await deskway(['call', device, tool, JSON.stringify(args)])).structuredContent;
  await check('deskway run returns output from the Linux host user', async () => {
    const result = await call('run', { command: 'id -un', wait_ms: 5000 });
    assert.equal(result.exit_code, 0);
    assert.equal(result.output.trim(), hostUser);
  });


  const files = '.files-surface:not([hidden])';
  const currentFolder = files + ' [aria-current="location"]';
  const entries = files + ' .files-row .files-name';
  const hostile = 'line\n\u202e<' + 'x'.repeat(200) + '>';
  const visibleHostile = 'line\\u{a}\\u{202e}<' + 'x'.repeat(200) + '>';
  const fileContents = 'contents-that-must-not-be-logged';
  const waitForFolder = (browser, name) => browser.until(
    `document.querySelector(${lit(currentFolder)})?.textContent === ${lit(name)} && !document.querySelector(${lit(files + ' > .notice:not([hidden])')})`,
    'the directory ' + name, 35000);
  const entryMenu = async (name, action) => {
    const selector = files + ' .files-list .more';
    const label = t('files.entryActions', { name });
    assert(await page.eval(`(() => {
      const button = [...document.querySelectorAll(${lit(selector)})].find((b) => b.getAttribute('aria-label') === ${lit(label)});
      if (!button) return false;
      button.scrollIntoView({ block: 'nearest' });
      return true;
    })()`), 'menu for ' + name);
    await sleep(100); // Let the scroll that places the row close any previously open menu.
    assert(await page.eval(`(() => {
      const button = [...document.querySelectorAll(${lit(selector)})].find((b) => b.getAttribute('aria-label') === ${lit(label)});
      if (!button) return false;
      button.click();
      return true;
    })()`), 'menu for ' + name);
    assert(await page.clickText(files + ' .menu:not([hidden]) button', t(action), { exact: true }));
  };
  const submitFileName = (name) => page.eval(`(() => {
    document.querySelector('.dialog.form input').value = ${lit(name)};
    document.querySelector('.dialog.form button[type="submit"]').click();
  })()`);
  const hasEntry = (name) => `[...document.querySelectorAll(${lit(entries)})].some((e) => e.textContent === ${lit(name)})`;

  await check('Files opens from the computer page in the host user home without a shell', async () => {
    await exec([...asHost, 'sh', '-ec', [
      'cd "$1"',
      'mkdir trial-many trial-unreadable',
      'mkdir trial-many/z-folder',
      'chmod 000 trial-unreadable',
      'printf %s "$3" > "$2"',
      'printf %s "$3" > trial-target',
      'ln -s trial-target trial-link',
      'i=4998; while [ "$i" -ge 0 ]; do : > "trial-many/$(printf %04d "$i")"; i=$((i-1)); done',
    ].join('\n'), 'fixtures', hostHome, hostile, fileContents], { timeout: 60000 });
    await detail(page, device);
    for (const [w, h] of [[390, 844], [1600, 900]]) {
      await page.metrics(w, h, false);
      const widths = await page.eval("[...document.querySelectorAll('.computer-actions button')].map((b) => b.getBoundingClientRect().width)");
      assert.equal(widths.length, 2);
      assert.equal(widths[0], widths[1]);
    }
    assert(await page.clickText('.computer-actions button', t('files.label'), { exact: true }));
    await waitForFolder(page, hostUser);
    assert.equal(await page.eval(`[...document.querySelectorAll(${lit(files + ' .files-path button')})].map((b) => b.textContent).join('|')`),
      ['/', ...hostHome.split('/').filter(Boolean)].join('|'));
    assert.equal(await page.eval("document.querySelectorAll('.terminal-surface .xterm').length"), 0);
    const processes = (await exec([host, 'ps', '-u', hostUser, '-o', 'tty=,comm='])).trim().split('\n');
    assert(processes.some((line) => line.endsWith('deskway-host')));
    assert(!processes.some((line) => /pts\/|\b(?:sh|bash|dash)\s*$/.test(line)), processes.join('\n'));
  });

  await check('directory pages list all 5,000 entries in order when scrolled, and breadcrumbs return home', async () => {
    assert(await page.clickText(entries, 'trial-many', { exact: true }));
    await waitForFolder(page, 'trial-many');
    const names = `[...document.querySelectorAll(${lit(entries)})].map((el) => el.textContent)`;
    const firstPage = await page.eval(names);
    assert(firstPage.length > 1 && firstPage.length < 5000, 'first page is bounded');
    assert.equal(firstPage[0], 'z-folder');
    for (let count = firstPage.length; count < 5000;) {
      await page.eval(`(() => {
        const list = document.querySelector(${lit(files + ' .files-list')});
        list.scrollTop = list.scrollHeight;
      })()`);
      await page.until(`(${names}).length > ${lit(count)}`, 'more directory entries', 12000);
      count = await page.eval(`(${names}).length`);
    }
    assert.deepEqual(await page.eval(names), ['z-folder', ...Array.from({ length: 4999 }, (_, i) => String(i).padStart(4, '0'))]);
    assert(await page.clickText(files + ' .files-path button', hostUser, { exact: true }));
    await waitForFolder(page, hostUser);
  });

  await check('the hostile name is escaped, truncated by layout, and can be confirmed and deleted', async () => {
    await page.metrics(390, 844, false);
    await page.until(hasEntry(visibleHostile), 'the escaped hostile name');
    assert(await page.eval(`[...document.querySelectorAll(${lit(entries)})].some((el) =>
      el.textContent === ${lit(visibleHostile)} && el.scrollWidth > el.clientWidth && !el.querySelector('x'))`));
    await entryMenu(visibleHostile, 'files.delete');
    assert.equal(await page.eval("document.querySelector('.dialog p').textContent"), t('files.confirmDelete', { name: visibleHostile }));
    assert(await page.clickText('.dialog .btn.danger', t('files.delete'), { exact: true }));
    await waitForFolder(page, hostUser);
    await page.until(`!(${hasEntry(visibleHostile)})`, 'the deleted entry leaves the list');
    await exec([...asHost, 'test', '!', '-e', hostHome + '/' + hostile]);
    assert.deepEqual(await page.eval('window.trialStyleViolations'), []);
    await page.metrics(1280, 800, false);
  });

  await check('new folder, rename, and confirmed delete work, and a non-empty folder is refused', async () => {
    await page.eval(`document.querySelector(${lit(files + ' .files-bar .more')}).click()`);
    assert(await page.clickText(files + ' .files-bar .menu-item', t('files.mkdir'), { exact: true }));
    await submitFileName('trial-created');
    await page.until(hasEntry('trial-created'), 'the new folder appears');
    await entryMenu('trial-created', 'common.rename');
    await submitFileName('trial-renamed');
    await page.until(hasEntry('trial-renamed'), 'the renamed folder appears');
    await exec([...asHost, 'touch', hostHome + '/trial-renamed/item']);
    await entryMenu('trial-renamed', 'files.delete');
    assert(await page.clickText('.dialog .btn.danger', t('files.delete'), { exact: true }));
    await page.until(`document.querySelector(${lit(files + ' > .notice')})?.textContent === ${lit(t('files.error.not_empty'))}`, 'non-empty folder refusal');
    await exec([...asHost, 'test', '-f', hostHome + '/trial-renamed/item']);
    await exec([...asHost, 'unlink', hostHome + '/trial-renamed/item']);
    await entryMenu('trial-renamed', 'files.delete');
    assert(await page.clickText('.dialog .btn.danger', t('files.delete'), { exact: true }));
    await waitForFolder(page, hostUser);
    await page.until(`!(${hasEntry('trial-renamed')})`, 'the deleted entry leaves the list');
    await entryMenu('trial-link', 'files.delete');
    assert(await page.clickText('.dialog .btn.danger', t('files.delete'), { exact: true }));
    await waitForFolder(page, hostUser);
    await page.until(`!(${hasEntry('trial-link')})`, 'the deleted entry leaves the list');
    assert.equal(await exec([...asHost, 'cat', hostHome + '/trial-target']), fileContents);
  });

  await check('retargeting a folder link keeps Delete in the listed folder and confirmation preserves spaces', async () => {
    const paddedName = '  trial  item  ';
    const original = hostHome + '/trial-original';
    const replacement = hostHome + '/trial-replacement';
    const link = hostHome + '/trial-current';
    await exec([...asHost, 'mkdir', original, replacement]);
    await exec([...asHost, 'touch', original + '/' + paddedName, replacement + '/' + paddedName]);
    await exec([...asHost, 'ln', '-s', original, link]);
    assert(await page.clickText(files + ' .files-bar button', t('files.refresh'), { exact: true }));
    await page.until(hasEntry('trial-current'), 'the link to the original folder');
    assert(await page.clickText(entries, 'trial-current', { exact: true }));
    await waitForFolder(page, 'trial-original');
    await exec([...asHost, 'ln', '-sfn', replacement, link]);
    await entryMenu(paddedName, 'files.delete');
    assert.equal(await page.eval("document.querySelector('.dialog p').textContent"), t('files.confirmDelete', { name: paddedName }));
    assert.equal(await page.eval("getComputedStyle(document.querySelector('.dialog p')).whiteSpaceCollapse"), 'preserve');
    assert(await page.clickText('.dialog .btn.danger', t('files.delete'), { exact: true }));
    await page.until(`document.querySelector(${lit(files + ' > .notice:not([hidden])')})?.textContent === ${lit(t('files.empty'))}`, 'the original folder is now empty');
    await exec([...asHost, 'test', '!', '-e', original + '/' + paddedName]);
    await exec([...asHost, 'test', '-f', replacement + '/' + paddedName]);
    assert(await page.clickText(files + ' .files-path button', hostUser, { exact: true }));
    await waitForFolder(page, hostUser);
  });

  await check('an unreadable folder gives an inline error and Files-only disconnect is plain', async () => {
    assert(await page.clickText(entries, 'trial-unreadable', { exact: true }));
    await page.until(`document.querySelector(${lit(files + ' > .notice')})?.textContent === ${lit(t('files.error.denied'))}`, 'in-place permission error');
    assert.equal(await page.eval(`document.querySelector(${lit(currentFolder)}).textContent`), 'trial-unreadable');
    await clickToolbar(page, '.toolbar button.end');
    assert.equal(await page.eval("document.querySelector('.dialog p').textContent"), t('files.leave'));
    assert.equal(await page.eval("document.querySelectorAll('.dialog button').length"), 2);
    assert(await page.clickText('.dialog button.primary', t('tb.disconnect'), { exact: true }));
    await page.until("!!document.querySelector('[data-page=\"detail\"]')", 'back to the computer');
    assert.equal(await page.eval(`document.querySelector(${lit(keptNotice)})`), null);
  });

  let firstShell;
  async function printedPid(page, marker) {
    await command(page, `printf '\\n%s%s\\n' ${marker} $$`);
    return pidOnScreen(page, marker);
  }
  async function pidOnScreen(page, marker) {
    const line = new RegExp('^' + marker + '(\\d+)$');
    return waitFor(async () => (await page.eval(rows)).map((r) => r.match(line)?.[1]).find(Boolean), marker + ' PID');
  }
  await check('Linux computer page opens a data-only terminal and physical keys run the shell as the host user', async () => {
    await openTerminal(page, device);
    await shellReady(page);
    assert.equal(await page.eval("document.querySelectorAll('.session video').length"), 0);
    await command(page, "PS1='trial$ '; printf '\\n\\033[31mTRIAL_USER %s\\033[0m\\n' \"$(id -un)\"; pwd");
    await page.until(`(${rows}).includes(${lit('TRIAL_USER ' + hostUser)}) && (${rows}).includes(${lit(hostHome)})`, 'shell identity and home');
    firstShell = await printedPid(page, 'FIRSTPID');
    assert.deepEqual(await page.eval('window.trialStyleViolations'), []);
  });


  await check('switching to Files and back keeps the same emulator, output and shell', async () => {
    await clickToolbar(page, '[data-session-view="files"]');
    await waitForFolder(page, hostUser);
    assert.equal(await page.eval("document.querySelector('.stage').hidden"), true);
    await clickToolbar(page, '[data-session-view="terminal"]');
    await shellReady(page);
    assert.equal(await page.eval("document.querySelector('.files-surface').hidden"), true);
    assert((await page.eval(rows)).includes('FIRSTPID' + firstShell));
    assert.equal(await printedPid(page, 'SWITCHPID'), firstShell);
  });


  const strip = '.file-transfer-strip';
  const transferName = strip + ' .xfer:not(.bad) .x-name';
  const transferFailure = strip + ' .xfer.bad .x-name';
  const partialPattern = '.deskway-upload-*.partial';
  const partials = async () => (await exec([...asHost, 'find', hostHome, '-maxdepth', '1', '-type', 'f', '-name', partialPattern])).trim();
  const largeName = 'transfer-30.bin';
  const fileSize = 30_000_000;
  const digest = (data) => createHash('sha256').update(data).digest('hex');
  const folderCheck = async (browser, timeout = 35000) => {
    await browser.until(
      `document.querySelector(${lit(currentFolder)})?.textContent === ${lit(hostUser)} && (${hasEntry('trial-target')})`,
      'host home listing', timeout);
  };
  const noPartials = async (name) => {
    await waitFor(async () => await partials() === '',
      'the staged upload to be removed', 90000);
    if (name) await exec([...asHost, 'test', '!', '-e', hostHome + '/' + name]);
  };
  const moving = (browser, name) => browser.until(
    `document.querySelector(${lit(transferName)})?.textContent.includes(${lit(name)}) && Number.parseInt(document.querySelector(${lit(strip + ' .x-pct')})?.textContent, 10) > 0`,
    'progress for ' + name, 60000);
  const selectFiles = async (browser, localFiles) => {
    await browser.send('Page.setInterceptFileChooserDialog', { enabled: true });
    await browser.until(
      `[...document.querySelectorAll(${lit(files + ' .files-bar button')})].some((b) => b.textContent === ${lit(t('files.upload'))} && !b.disabled && !b.hidden)`,
      'the enabled Upload button');
    const at = await browser.eval(`(() => {
      const button = [...document.querySelectorAll(${lit(files + ' .files-bar button')})]
        .find((b) => b.textContent === ${lit(t('files.upload'))});
      const { x, y, width, height } = button.getBoundingClientRect();
      return [x + width / 2, y + height / 2];
    })()`);
    await click(browser, at);
    const { result: { root } } = await browser.send('DOM.getDocument');
    const { result: { nodeId } } = await browser.send('DOM.querySelector', { nodeId: root.nodeId, selector: files + ' input[type=file]' });
    const result = await browser.send('DOM.setFileInputFiles', { nodeId, files: localFiles });
    assert(!result.error, result.error?.message);
  };
  // Delay reads at the browser for timing checks, or hold a read halfway through for cancellation.
  const paceUploads = (browser, pause) => browser.eval(`(() => {
    const slice = File.prototype.slice;
    let resume;
    const paused = new Promise((resolve) => { resume = resolve; });
    File.prototype.slice = function(start, end) {
      const blob = slice.call(this, start, end);
      const read = blob.arrayBuffer.bind(blob);
      blob.arrayBuffer = async () => {
        if (${lit(pause)} && start >= this.size / 2) await paused;
        else if (!${lit(pause)}) await new Promise((r) => setTimeout(r, 8));
        return read();
      };
      return blob;
    };
    window.restoreUploadReads = () => { File.prototype.slice = slice; };
    window.resumeTrialUpload = () => { window.restoreUploadReads(); resume(); };
  })()`);
  const halfUpload = async (browser, name) => {
    await browser.until(`
      document.querySelector(${lit(transferName)})?.textContent.includes(${lit(name)}) &&
      Number.parseInt(document.querySelector(${lit(strip + ' .x-pct')})?.textContent, 10) >= 50`, 'the upload halfway point', 60000);
    const partial = await partials();
    assert.match(partial, /^\/[^\r\n]+\.deskway-upload-[a-f0-9]{32}\.partial$/);
    assert(partial.startsWith(hostHome + '/.deskway-upload-'));
    return partial;
  };
  const startDroppedUpload = async (browser, name) => {
    await paceUploads(browser, true);
    await browser.eval(`(() => {
      const dataTransfer = new DataTransfer();
      dataTransfer.items.add(new File([new Uint8Array(${lit(fileSize)})], ${lit(name)}));
      document.querySelector(${lit(files)}).dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer }));
    })()`);
    const partial = await halfUpload(browser, name);
    await browser.eval('window.restoreUploadReads()');
    return partial;
  };
  const failure = (browser, name, reason) => browser.until(
    `[...document.querySelectorAll(${lit(transferFailure)})].some((row) => row.textContent === ${lit(t('files.transfer.failure', { name, reason }))})`,
    reason, 60000);
  const digestOnHost = async (name) => (await exec([...asHost, 'sha256sum', hostHome + '/' + name])).trim().split(/\s+/)[0];

  const large = Buffer.alloc(fileSize, 0x5d);
  large.write(fileContents);
  const expectedHash = digest(large);
  const largeLocal = join(folder, largeName);
  const downloads = join(folder, 'downloads');
  mkdirSync(downloads);
  writeFileSync(largeLocal, large);

  await check('30 MB reaches the host with its SHA-256, appears in Files, and leaves no partial', async () => {
    await clickToolbar(page, '[data-session-view="files"]');
    await folderCheck(page);
    await paceUploads(page, false);
    await selectFiles(page, [largeLocal]);
    await moving(page, largeName);
    await check('switching to Terminal and back keeps the same transfer visible and running', async () => {
      await clickToolbar(page, '[data-session-view="terminal"]');
      assert(await page.eval(`document.querySelector(${lit(transferName)})?.textContent.includes(${lit(largeName)})`));
      await check('typing during an upload produces output within 2,000 ms', async () => {
        const since = Date.now();
        await command(page, 'echo TRANSFERKEY');
        await page.until(`(${rows}).includes('TRANSFERKEY')`, 'terminal output during transfer', 2000);
        const elapsed = Date.now() - since;
        record('typing and echo took ' + elapsed + ' ms\n');
        assert(elapsed <= 2000);
      });
      await clickToolbar(page, '[data-session-view="files"]');
      assert(await page.eval(`document.querySelector(${lit(transferName)})?.textContent.includes(${lit(largeName)})`));
      await check('a folder lists during an upload within 2,000 ms', async () => {
        const marker = 'trial-responsive.txt';
        await exec([...asHost, 'touch', hostHome + '/' + marker]);
        const since = Date.now();
        assert(await page.clickText(files + ' .files-bar button', t('files.refresh'), { exact: true }));
        await page.until(hasEntry(marker), 'the new file in the refreshed folder', 2000);
        const elapsed = Date.now() - since;
        record('folder refresh took ' + elapsed + ' ms\n');
        assert(elapsed <= 2000);
        await moving(page, largeName);
      });
    });
    await page.until(hasEntry(largeName), 'the committed upload in the listing', 90000);
    assert.equal(await digestOnHost(largeName), expectedHash);
    await noPartials();
    await page.eval('window.restoreUploadReads()');
  });

  const ordered = ['queue-c.txt', 'queue-a.txt', 'queue-b.txt'];
  await check('three files chosen together arrive in picker order', async () => {
    const paths = ordered.map((name) => {
      const path = join(folder, name);
      writeFileSync(path, name);
      return path;
    });
    await selectFiles(page, paths);
    await page.until(ordered.map(hasEntry).join(' && '), 'all three uploads in Files', 60000);
    const ends = foreground.stderr.split('\n').filter((line) => line.includes('direction=upload') && line.includes('outcome=done'));
    const order = ends.filter((line) => ordered.some((name) => line.includes('/' + name + '"')))
      .map((line) => ordered.find((name) => line.includes('/' + name + '"')));
    assert.deepEqual(order, ordered);
  });

  const conflict = async (name) => page.until(
    `document.querySelector('.dialog h2')?.textContent === ${lit(t('files.transfer.conflictTitle', { name }))}`,
    'the conflict for ' + name);
  const choose = async (action) => assert(await page.clickText('.dialog button', t('files.transfer.' + action), { exact: true }));
  const expand = async () => page.eval(`(() => {
    const toggle = document.querySelector(${lit(strip + ' > button')});
    if (toggle.getAttribute('aria-expanded') === 'false') toggle.click();
  })()`);
  const collapse = async () => page.eval(`(() => {
    const toggle = document.querySelector(${lit(strip + ' > button')});
    if (toggle?.getAttribute('aria-expanded') === 'true') toggle.click();
  })()`);
  const verified = (name) => page.until(
    `[...document.querySelectorAll(${lit(strip + ' .xfer.ok .x-name')})].some((row) => row.textContent === ${lit(name)})`,
    'the verified row for ' + name, 90000);

  await check('Skip preserves the existing file and sends no upload data', async () => {
    const before = await page.eval('window.trialFileTraffic.at(-1).up');
    await selectFiles(page, [largeLocal]);
    await conflict(largeName);
    await noPartials();
    await choose('skip');
    await page.until(`!document.querySelector(${lit(transferName)})`, 'the skipped transfer to end');
    assert.deepEqual(await page.eval('window.trialFileTraffic.at(-1).up'), before);
    assert.equal(await digestOnHost(largeName), expectedHash);
  });

  const replacement = 'replace-30.bin';
  const late = 'transfer-late.txt';
  const directoryConflict = 'transfer-directory';
  const corrupt = 'transfer-corrupt.bin';
  const original = Buffer.from('the old destination');
  const oldHash = digest(original);
  const putOriginal = (name) => exec([...asHost, 'tee', hostHome + '/' + name], { input: original });
  const local = (name, data) => {
    const path = join(folder, name);
    writeFileSync(path, data);
    return path;
  };

  await check('Replace keeps the old content until the staged upload verifies', async () => {
    await putOriginal(replacement);
    await paceUploads(page, true);
    await selectFiles(page, [local(replacement, large)]);
    await conflict(replacement);
    await noPartials();
    assert(await page.eval("document.querySelector('.dialog button.danger') !== null"));
    await choose('replace');
    await halfUpload(page, replacement);
    assert.equal(await digestOnHost(replacement), oldHash);
    await page.eval('window.resumeTrialUpload()');
    await waitFor(async () => (await digestOnHost(replacement)) === expectedHash, 'verified replacement', 90000);
    await noPartials();
  });

  await check('the batch choice replaces all three colliding files', async () => {
    const changed = ordered.map((name) => local(name, 'new ' + name));
    await selectFiles(page, changed);
    await conflict(ordered[0]);
    assert(await page.eval(`(() => {
      const box = document.querySelector('.dialog input[type=checkbox]');
      box?.click();
      return box?.checked;
    })()`));
    await choose('replace');
    await expand();
    for (const name of ordered) {
      await waitFor(async () => (await digestOnHost(name)) === digest('new ' + name), 'replaced ' + name);
    }
    await collapse();
    await noPartials();
  });

  await check('a directory of the same name is refused before staging', async () => {
    await exec([...asHost, 'mkdir', hostHome + '/' + directoryConflict]);
    await selectFiles(page, [local(directoryConflict, 'file')]);
    await failure(page, directoryConflict, t('files.transfer.notRegular'));
    await exec([...asHost, 'test', '-d', hostHome + '/' + directoryConflict]);
    await noPartials();
  });

  await check('a name created during transfer prompts after all bytes arrive', async () => {
    await paceUploads(page, true);
    await selectFiles(page, [local(late, large)]);
    await halfUpload(page, late);
    await putOriginal(late);
    await page.eval('window.resumeTrialUpload()');
    await conflict(late);
    assert.equal(await digestOnHost(late), oldHash);
    await choose('skip');
    await noPartials();
    assert.equal(await digestOnHost(late), oldHash);
  });

  await check('a corrupted staged upload fails verification and Try again works', async () => {
    await putOriginal(corrupt);
    await paceUploads(page, true);
    await selectFiles(page, [local(corrupt, large)]);
    await conflict(corrupt);
    await choose('replace');
    const partial = await halfUpload(page, corrupt);
    // Alter a byte already acknowledged by the host without changing the staged length.
    await exec([...asHost, 'dd', 'if=/dev/zero', 'of=' + partial, 'bs=1', 'count=1', 'conv=notrunc']);
    await page.eval('window.resumeTrialUpload()');
    await failure(page, corrupt, t('files.transfer.integrity'));
    await noPartials();
    assert.equal(await digestOnHost(corrupt), oldHash);
    await expand();
    assert(await page.eval(`(() => {
      const row = [...document.querySelectorAll(${lit(strip + ' .xfer.bad')})]
        .find((item) => item.querySelector('.x-name')?.textContent === ${lit(t('files.transfer.failure', { name: corrupt, reason: t('files.transfer.integrity') }))});
      const retry = [...row.querySelectorAll('button')].find((button) => button.textContent === ${lit(t('files.transfer.tryAgain'))});
      retry?.click();
      return !!retry;
    })()`));
    await conflict(corrupt);
    await choose('replace');
    await verified(corrupt);
    assert.equal(await digestOnHost(corrupt), expectedHash);
    await collapse();
  });

  const cancelledName = 'cancelled-upload.bin';
  await check('cancelling an upload halfway leaves no partial', async () => {
    await startDroppedUpload(page, cancelledName);
    const label = t('common.cancel');
    assert(await page.eval(`(() => {
      const cancel = [...document.querySelectorAll(${lit(strip + ' button')})].find((b) => b.getAttribute('aria-label') === ${lit(label)});
      cancel?.click();
      return !!cancel;
    })()`));
    await noPartials(cancelledName);
  });

  await check('Chrome downloads 30 MB through the in-memory path with the correct SHA-256', async () => {
    await page.eval("Object.defineProperty(window, 'showSaveFilePicker', { value: undefined, configurable: true })");
    const browser = await harness.browserPage();
    const result = await browser.send('Browser.setDownloadBehavior', {
      behavior: 'allow', browserContextId: page.context, downloadPath: downloads,
    });
    assert(!result.error, result.error?.message);
    await entryMenu(largeName, 'files.download');
    const downloaded = join(downloads, largeName);
    await waitFor(() => existsSync(downloaded), 'the completed Chrome download', 90000);
    assert.equal(digest(readFileSync(downloaded)), expectedHash);
    await page.until(`!document.querySelector(${lit(transferName)})`, 'download completion');
  });

  const resumedUpload = 'resume-upload-30.bin';
  const queuedUpload = 'resume-waiting.txt';
  const resumedDownload = 'resume-download-30.bin';
  const readmittedUpload = 'resume-readmission-30.bin';
  const interrupted = () => page.until(
    `document.querySelector(${lit(strip)})?.textContent.includes(${lit(t('files.transfer.interrupted'))})`,
    'the interrupted transfer', 28000);
  const channelCount = () => page.eval('window.trialFileTraffic.length');
  const peerCount = () => page.eval('window.trialPeers.length');
  const waitForNewChannel = (before) => page.until(
    `window.trialFileTraffic.length === ${lit(before + 1)} && window.trialFileChannel.readyState === 'open'`,
    'the recovered transfer channel', 90000);
  // Both peers use TURN over TCP. Drop its input in their shared namespace and keep signaling.
  const dropTurn = async (run) => {
    const rule = ['INPUT', '-p', 'tcp', '--dport', String(turn), '-j', 'DROP'];
    await exec([host, 'iptables', '-I', ...rule]);
    try { await run(); }
    finally { await exec([host, 'iptables', '-D', ...rule]); }
  };
  const checkTraffic = async (direction, before) => {
    const { offset, bytes } = await page.eval(`Object.values(window.trialFileTraffic[${lit(before)}][${lit(direction)}])[0]`);
    assert(offset > 0 && bytes > 0 && bytes < fileSize);
    assert.equal(bytes, fileSize - offset);
    record(`${direction} resumed at byte ${offset}, sent ${bytes} of ${fileSize} bytes on the new channel\n`);
  };

  await check('a 30 MB upload continues after ICE restart and its waiting file is preserved', async () => {
    await paceUploads(page, true);
    const before = await channelCount();
    const peers = await peerCount();
    const waiting = () => page.until(`
      [...document.querySelectorAll(${lit(strip + ' .xfer')})].some((row) =>
        row.querySelector('.x-name')?.textContent === ${lit(queuedUpload)}
        && row.querySelector('.x-detail')?.textContent === ${lit(t('files.transfer.waiting'))})`, 'the waiting file');
    await selectFiles(page, [local(resumedUpload, large), local(queuedUpload, 'still queued')]);
    await halfUpload(page, resumedUpload);
    await expand();
    await waiting();
    await verified(largeName);
    await failure(page, directoryConflict, t('files.transfer.notRegular'));
    await dropTurn(async () => {
      await interrupted();
      await waiting();
      await page.eval('window.resumeTrialUpload()');
    });
    await waitForNewChannel(before);
    await verified(resumedUpload);
    await verified(queuedUpload);
    await checkTraffic('up', before);
    assert.equal(await peerCount(), peers, 'ICE recovery kept the peer connection');
    assert.equal(await digestOnHost(resumedUpload), expectedHash);
    assert.equal(await digestOnHost(queuedUpload), digest('still queued'));
    await noPartials();
    await collapse();
  });

  await check('a 30 MB download continues after ICE restart and verifies', async () => {
    await exec([...asHost, 'cp', hostHome + '/' + largeName, hostHome + '/' + resumedDownload]);
    assert(await page.clickText(files + ' .files-bar button', t('files.refresh'), { exact: true }));
    await page.until(hasEntry(resumedDownload), 'the resumable download');
    const before = await channelCount();
    const peers = await peerCount();
    await page.eval(`(() => {
      const send = RTCDataChannel.prototype.send;
      RTCDataChannel.prototype.send = function(data) {
        if (this.label === 'file-transfer' && typeof data === 'string') {
          const message = JSON.parse(data);
          if (message.t === 'ack' && message.offset >= ${lit(fileSize / 2)}) {
            window.trialPausedDownload = true;
            return;
          }
        }
        send.call(this, data);
      };
      window.restoreTrialDownload = () => { RTCDataChannel.prototype.send = send; };
    })()`);
    try {
      await entryMenu(resumedDownload, 'files.download');
      await page.until('window.trialPausedDownload', 'half of the download acknowledged', 60000);
      await dropTurn(async () => {
        await interrupted();
        await page.eval('window.restoreTrialDownload()');
      });
    } finally {
      await page.eval('window.restoreTrialDownload()');
    }
    await waitForNewChannel(before);
    await expand();
    await verified(resumedDownload);
    await checkTraffic('down', before);
    assert.equal(await peerCount(), peers, 'ICE recovery kept the peer connection');
    const downloaded = join(downloads, resumedDownload);
    await waitFor(() => existsSync(downloaded), 'the completed resumed download');
    assert.equal(digest(readFileSync(downloaded)), expectedHash);
    await collapse();
  });

  await check('an upload continues through a fresh admission when the control channel ends', async () => {
    await paceUploads(page, true);
    const before = await channelCount();
    const peers = await peerCount();
    await selectFiles(page, [local(readmittedUpload, large)]);
    await halfUpload(page, readmittedUpload);
    await page.eval('window.trialControlChannel.close()');
    await waitForNewChannel(before);
    await page.eval('window.resumeTrialUpload()');
    await expand();
    await verified(readmittedUpload);
    assert.equal(await peerCount(), peers + 1, 'readmission opened a new peer connection');
    await checkTraffic('up', before);
    assert.equal(await digestOnHost(readmittedUpload), expectedHash);
    await noPartials();
    await collapse();
  });

  const tooLarge = 'download-over-bound.bin';
  const growing = 'download-growing.bin';
  await check('a download over 64 MiB is refused before any data arrives', async () => {
    await exec([...asHost, 'truncate', '-s', String(64 * 1024 * 1024 + 1), hostHome + '/' + tooLarge]);
    await exec([...asHost, 'truncate', '-s', String(fileSize), hostHome + '/' + growing]);
    assert(await page.clickText(files + ' .files-bar button', t('files.refresh'), { exact: true }));
    await page.until(`${hasEntry(tooLarge)} && ${hasEntry(growing)}`, 'the download fixtures');
    await page.eval(`(() => {
      window.trialDownloadBytes = 0;
      window.trialFileChannel.addEventListener('message', ({ data }) => {
        if (data instanceof ArrayBuffer) window.trialDownloadBytes += data.byteLength;
      });
    })()`);
    await entryMenu(tooLarge, 'files.download');
    await failure(page, tooLarge, t('files.transfer.limit', { limit: 64 }));
    assert.equal(await page.eval('window.trialDownloadBytes'), 0);
    assert(!existsSync(join(downloads, tooLarge)));
  });

  await check('a file that grows during download fails with the change message', async () => {
    await page.eval(`(() => {
      const native = RTCDataChannel.prototype.send;
      const held = [];
      RTCDataChannel.prototype.send = function(data) {
        if (this.label === 'file-transfer' && typeof data === 'string' && JSON.parse(data).t === 'ack') {
          held.push([this, data]);
        } else native.call(this, data);
      };
      window.heldDownloadAcks = () => held.length;
      window.releaseDownloadAcks = () => {
        RTCDataChannel.prototype.send = native;
        held.forEach(([channel, data]) => native.call(channel, data));
      };
    })()`);
    await entryMenu(growing, 'files.download');
    await page.until('window.heldDownloadAcks() === 4', 'the first download credit window');
    await exec([...asHost, 'truncate', '-s', String(fileSize + 1), hostHome + '/' + growing]);
    await page.eval('window.releaseDownloadAcks()');
    await failure(page, growing, t('files.transfer.changed'));
    assert(!existsSync(join(downloads, growing)));
  });

  await check('listing a folder sweeps an old partial and keeps a young one', async () => {
    const old = '.deskway-upload-' + 'a'.repeat(32) + '.partial';
    const young = '.deskway-upload-' + 'b'.repeat(32) + '.partial';
    const sweep = hostHome + '/trial-sweep';
    await exec([...asHost, 'mkdir', sweep]);
    await exec([...asHost, 'touch', '-d', '2 hours ago', sweep + '/' + old]);
    await exec([...asHost, 'touch', sweep + '/' + young]);
    assert(await page.clickText(files + ' .files-bar button', t('files.refresh'), { exact: true }));
    await page.until(hasEntry('trial-sweep'), 'the sweep fixture');
    assert(await page.clickText(entries, 'trial-sweep', { exact: true }));
    await waitForFolder(page, 'trial-sweep');
    await exec([...asHost, 'test', '!', '-e', sweep + '/' + old]);
    await exec([...asHost, 'test', '-f', sweep + '/' + young]);
    await clickToolbar(page, '[data-session-view="terminal"]');
  });

  await check('holding Ctrl and pressing C on the on-screen keyboard interrupts sleep and its keys type a command', async () => {
    await command(page, "printf '\\nRUN%s\\n' NING; sleep 60");
    await page.until(`(${rows}).includes('RUNNING')`, 'sleep started');
    await clickToolbar(page, keyboardButton);
    await page.until("!!document.querySelector('.keypad')", 'on-screen keyboard visible');
    const key = (code) => centre(page, `.keypad [data-code="${code}"]`);
    const ctrl = await key('ControlLeft');
    await touch(page, 'touchStart', [ctrl]);
    await page.until("document.querySelector('.keypad [data-code=ControlLeft]').getAttribute('aria-pressed') === 'true'", 'on-screen Ctrl held');
    await touch(page, 'touchStart', [ctrl, await key('KeyC')]);
    await touch(page, 'touchEnd', []);
    await prompt(page, 3000);
    for (const code of ['KeyE', 'KeyC', 'KeyH', 'KeyO', 'Space', 'KeyO', 'KeyS', 'KeyK', 'Enter']) await tap(page, await key(code));
    await page.until(`(${rows}).includes('osk')`, 'output of the command typed on the on-screen keyboard');
    await page.eval("document.querySelector('.kb-hide').click()");
  });

  await check('a 320 KiB paste into cat has the same SHA-256 on the host', async () => {
    const payload = Array.from({ length: 4096 }, (_, i) => String(i).padStart(4, '0') + 'abcdef'.repeat(12) + 'abc\n').join('');
    assert.equal(Buffer.byteLength(payload), 320 * 1024);
    await command(page, "stty -echo; printf '\\nPASTE%s\\n' READY; cat > trial-paste; stty echo");
    await page.until(`(${rows}).includes('PASTEREADY')`, 'cat ready for the paste');
    await paste(page, payload);
    const { press } = keyboardOf(page);
    await press('ControlLeft', true);
    await press('KeyD', true, { windowsVirtualKeyCode: 68 });
    await press('KeyD', false, { windowsVirtualKeyCode: 68 });
    await press('ControlLeft', false);
    await prompt(page, 30000);
    const [hash] = (await exec([...asHost, 'sha256sum', hostHome + '/trial-paste'])).split(/\s+/, 1);
    assert.equal(hash, createHash('sha256').update(payload).digest('hex'));
  });

  await check('resizing the browser changes stty size', async () => {
    // Earlier checks fixed the viewport's size; the window decides it again from here.
    await page.send('Emulation.clearDeviceMetricsOverride');
    const browserPage = await harness.browserPage();
    const { windowId } = (await browserPage.send('Browser.getWindowForTarget', { targetId: page.targetId })).result;
    async function measure(width, height, marker) {
      const resized = await browserPage.send('Browser.setWindowBounds', { windowId, bounds: { width, height } });
      assert(!resized.error, resized.error?.message);
      await sleep(500);
      await command(page, `printf '\\nSIZE%s ' ${marker}; stty size`);
      const regex = new RegExp(`^SIZE${marker} (\\d+ \\d+)$`);
      return waitFor(async () => (await page.eval(rows)).map((r) => r.match(regex)?.[1]).find(Boolean), 'stty size');
    }
    const before = await measure(1200, 850, 1);
    const after = await measure(560, 600, 2);
    assert.notEqual(after, before);
    record(`  stty size ${before} -> ${after}\n`);
  });

  await check('yes runs for two seconds, the page responds in one second, and Ctrl-C restores the prompt within three seconds', async () => {
    await command(page, 'yes');
    await page.until(`(${rows}).includes('y')`, 'output from yes');
    const flood = Date.now();
    await sleep(1000);
    const frame = Date.now();
    await page.eval('new Promise((resolve) => requestAnimationFrame(() => resolve(true)))');
    assert(Date.now() - frame < 1000, 'browser frame took at least 1 second');
    await sleep(Math.max(0, 2000 - (Date.now() - flood)));
    const stop = Date.now();
    await interrupt(page);
    await prompt(page, 3000);
    const elapsed = Date.now() - stop;
    assert(elapsed < 3000, 'prompt took at least 3 seconds');
    record(`  prompt returned in ${elapsed} ms\n`);
  });

  await check('exit status is shown after final output and opening again starts a fresh shell', async () => {
    await command(page, "printf '\\nLAST%s\\n' LINE; exit 17");
    await page.until(`document.querySelector('.terminal-status')?.textContent === ${lit(t('terminal.exit', { code: 17 }))}`, 'exit status');
    assert((await page.eval(rows)).includes('LASTLINE'));
    await disconnect(page);
    assert.equal(await page.eval(`!!document.querySelector(${lit(keptNotice)})`), false);
    await processGone(firstShell);
    await openTerminal(page, device);
    await shellReady(page);
    const next = await printedPid(page, 'NEXTPID');
    assert.notEqual(next, firstShell);
  });

  await check('another tab of the same browser is asked to take over its established terminal', async () => {
    const tab = await newBrowser(harness, url, true);
    await tab.until(`d?.devices().find((x) => x.id === ${lit(device)})?.controller?.device === true`, 'the same browser holds the terminal');
    await tab.v(`d.interceptSignals((m) => {
      if (m.type === 'bye' && m.reason === 'busy') window.trialBusy = true;
      return m;
    })`);
    await openTerminal(tab, device);
    await tab.until(`window.trialBusy && document.querySelector('.dialog h2')?.textContent === ${lit(t('force.deviceTitle'))}`, 'host asks whether to take over');
    assert(await tab.clickText('.dialog button', t('common.cancel'), { exact: true }));
    await harness.closePage(tab);
    await page.send('Page.bringToFront');
    await shellReady(page);
  });

  let jobNumber = 0;
  async function startJob(page) {
    const marker = 'JOB' + ++jobNumber + '-';
    await command(page, `trap '' HUP; sleep 300 </dev/null >/dev/null 2>&1 & printf '\\n%s%s\\n' ${marker} $!`);
    const pid = await pidOnScreen(page, marker);
    await exec([host, 'test', '-d', '/proc/' + pid]);
    return pid;
  }
  async function processGone(pid) {
    await waitFor(async () => await start('docker', ['exec', host, 'test', '!', '-d', '/proc/' + pid], {
      quiet: true, timeout: 5000,
    }).done === 0, `process ${pid} to go away`, 10000);
  }

  let counter;
  const count = () => exec([host, 'tail', '-n', '1', hostHome + '/trial-count']).then((s) => Number(s.trim()));
  await check('Disconnect keeps the shell running and Return replays counter lines from the time away', async () => {
    await command(page, "PS1='trial$ '; : > trial-count; trap '' HUP; (n=0; while :; do n=$((n+1)); printf '\\nCOUNT%s\\n' \"$n\"; echo \"$n\" >> trial-count; sleep 1; done) & printf '\\nCOUNTER%s\\n' $!");
    counter = await pidOnScreen(page, 'COUNTER');
    await disconnect(page);
    await kept(page, keeper);
    assert.match(await page.eval(`document.querySelector(${lit(keptNotice)}).textContent`), /just now/i);
    const before = await count();
    const missed = await waitFor(async () => { const n = await count(); return n >= before + 2 && n; }, 'the detached counter to advance');
    await returnTerminal(page);
    await page.until(`(${rows}).includes(${lit('COUNT' + missed)})`, 'a line printed while detached');
    await page.until(`(${rows}).some((r) => /^COUNT\\d+$/.test(r) && Number(r.slice(5)) > ${lit(missed)})`, 'live counter output');
    await exec([host, 'test', '-d', '/proc/' + counter]);
  });

  await check('closing the browser page abruptly leaves the same counter available from a new page', async () => {
    const before = await count();
    await clickToolbar(page, '[data-session-view="files"]');
    await folderCheck(page);
    const partial = await startDroppedUpload(page, 'closed-upload.bin');
    await harness.closePage(page);
    page = await newBrowser(harness, url, true);
    await page.until('!!d?.me() && d.socketOpen()', 'the same browser on a new page');
    await detail(page, device);
    await kept(page, keeper, 90000);
    const missed = await waitFor(async () => { const n = await count(); return n > before && n; }, 'the counter to advance while the page was closed');
    await returnTerminal(page);
    await page.until(`(${rows}).includes(${lit('COUNT' + missed)})`, 'counter output from the time the page was closed');
    await check('closing the page mid-upload leaves the partial to the age sweep', async () => {
      await exec([...asHost, 'test', '-f', partial]);
      await exec([...asHost, 'touch', '-d', '2 hours ago', partial]);
      await clickToolbar(page, '[data-session-view="files"]');
      await folderCheck(page);
      assert(await page.eval(`!document.querySelector(${lit(strip + ' > button')})`));
      await noPartials('closed-upload.bin');
      await clickToolbar(page, '[data-session-view="terminal"]');
    });
    await command(page, 'kill ' + counter + '; wait ' + counter);
    await processGone(counter);
    await command(page, 'true');
    await prompt(page, 5000);
  });

  await check('detached newline-free output exceeds 256 KiB and returns with a drop notice and a prompt', async () => {
    await command(page, "PS1='trial$ ' bash --noprofile --norc -i");
    await prompt(page, 5000);
    // Readline enables bracketed paste when it displays the prompt. Keep that prompt
    // active while the background job floods output, so Readline does not enable it again.
    await command(page, "bind 'set enable-bracketed-paste on'; printf '\\nFLOOD%s\\n' READY; (while test ! -f trial-flood; do sleep 1; done; head -c 400000 /dev/zero | tr '\\000' '\\r'; printf '\\nFLOOD%s\\ntrial$ ' DONE; touch trial-flood-done) &");
    await page.until(`(${rows}).includes('FLOODREADY')`, 'the flood command waiting');
    await prompt(page, 5000);
    await disconnect(page);
    await kept(page, keeper);
    await exec([...asHost, 'touch', hostHome + '/trial-flood']);
    await waitFor(async () => await start('docker', ['exec', host, 'test', '-f', hostHome + '/trial-flood-done'], {
      quiet: true, timeout: 5000,
    }).done === 0, 'the detached flood to finish', 10000);
    await returnTerminal(page);
    await prompt(page, 10000);
    await page.until(`document.querySelector('.terminal-status:not([hidden])')?.textContent === ${lit(t('terminal.dropped'))}`, 'the drop notice');
    assert((await page.eval(rows)).includes('FLOODDONE'));
  });

  await check('bracketed paste still protects multiline input after the detached flood', async () => {
    const files = ['trial-paste-first', 'trial-paste-second'];
    const lines = files.map((file) => 'touch ' + file);
    await paste(page, lines.join('\n') + '\n');
    await page.until(`${lit(lines)}.every((line) => (${rows}).some((row) => row.endsWith(line)))`, 'both pasted lines');
    await interrupt(page);
    await prompt(page, 5000);
    await command(page, "printf '\\nPASTE%s\\n' CANCELLED");
    await page.until(`(${rows}).includes('PASTECANCELLED')`, 'the shell to accept input after cancelling');
    for (const file of files) {
      await exec([...asHost, 'test', '!', '-e', hostHome + '/' + file]);
    }
    await command(page, 'exit');
    await prompt(page, 5000);
  });

  await check('top redraws after the emulator catches up, before its next scheduled update', async () => {
    await command(page, 'top -d 60');
    const clock = async () => (await page.eval(rows)).find((r) => r.startsWith('top - '))?.slice(6, 14);
    const before = await waitFor(clock, 'top to draw');
    await disconnect(page);
    await kept(page, keeper);
    await sleep(2100);
    await returnTerminal(page);
    await waitFor(async () => { const after = await clock(); return after && after !== before; }, 'top to redraw', 7000);
    await interrupt(page);
    await prompt(page, 5000);
    await disconnect(page);
  });

  const taker = 'Terminal Taker';
  const other = await newBrowser(harness, url);
  const hostOnline = `d?.devices().some((x) => x.id === ${lit(device)} && x.online)`;
  await check('an untrusted browser is told how to get approval and no terminal opens', async () => {
    await other.until("!!document.querySelector('form.login')", 'the second browser login');
    await signIn(other, { user, password: secret });
    await nameThisDevice(other, taker);
    await superIn(other, superPassword);
    await other.until(hostOnline, 'the second browser knows the host');
    await openTerminal(other, device);
    await other.until(`document.querySelector('.dialog h2')?.textContent === ${lit(t('notAdded.untrustedTitle'))}`,
      'the headless device approval instructions');
    assert.equal(await other.eval("document.querySelector('.dialog p').textContent"), t('notAdded.browserApproval', {
      devices: t('devices.title'), current: t('devices.tagThis'), signOut: t('devices.signOut'),
    }));
    assert(await other.eval("!!document.querySelector('[data-page=\"detail\"]') && !document.querySelector('.terminal-surface')"));
  });

  await check('the first browser approves the second with matching codes', async () => {
    assert(await other.clickText('.dialog button', t('devices.title'), { exact: true }));
    const menu = `button[aria-label=${lit(t('computer.menu', { name: taker }))}]`;
    await other.until(`!!document.querySelector(${lit(menu)})`, 'the second browser in My devices');
    await other.eval(`document.querySelector(${lit(menu)}).click()`);
    assert(await other.clickText('.menu:not([hidden]) button', t('devices.signOut'), { exact: true }));
    assert(await other.clickText('.dialog button', t('devices.signOut'), { exact: true }));
    await other.until("!!document.querySelector('form.login')", 'the second browser signed out');
    await signIn(other, { user, password: secret });
    await other.until("!!document.querySelector('.wait-step .code')", 'the second browser’s approval code');
    const digits = await other.eval("document.querySelector('.wait-step .code').textContent.replace(/\\D/g, '')");
    assert.match(digits, /^\d{6}$/);
    await page.send('Page.bringToFront');
    await page.until(`document.querySelector('.dialog h2')?.textContent === ${lit(t('approve.deviceTitle', { name: taker }))} && !!document.querySelector('.dialog .code')`,
      'the first browser’s approval request');
    assert.equal(await page.eval("document.querySelector('.dialog .code').textContent.replace(/\\D/g, '')"), digits);
    assert(await page.clickText('.dialog button', t('common.approve'), { exact: true }));
    await page.until(`document.querySelector('.dialog h2')?.textContent === ${lit(t('approve.addedTitle'))}`, 'the signed browser endorsement');
    assert(await page.clickText('.dialog button', t('common.ok'), { exact: true }));
    await other.send('Page.bringToFront');
    await other.until(hostOnline, 'the second browser signed in after approval');
  });

  let shell, job;
  await check('a second approved browser sees the notice and takes over the same shell and job', async () => {
    await detail(other, device);
    await kept(other, keeper);
    await page.send('Page.bringToFront');
    await openTerminal(page, device);
    await shellReady(page);
    shell = await printedPid(page, 'BEFOREPID');
    job = await startJob(page);
    await clickToolbar(page, '[data-session-view="files"]');
    await folderCheck(page);
    await startDroppedUpload(page, 'takeover-upload.bin');
    await other.send('Page.bringToFront');
    await other.until(`d.devices().find((x) => x.id === ${lit(device)})?.controller?.name`, 'the first browser holds the computer');
    await openTerminal(other, device);
    await other.until(`[...document.querySelectorAll('.dialog button')].some((b) => b.textContent === ${lit(t('force.accountGo'))})`, 'takeover prompt');
    assert(await other.clickText('.dialog button', t('force.accountGo'), { exact: true }));
    await shellReady(other);
    assert.equal(await printedPid(other, 'AFTERPID'), shell);
    await exec([host, 'test', '-d', '/proc/' + job]);
    await page.until(`!document.querySelector('.terminal-surface') && document.querySelector('.dialog h2')?.textContent === ${lit(t('session.takenTitle', { name: taker }))}`,
      'the first browser was taken over');
    await check('a takeover ends the first browser’s transfer and removes its partial', async () => {
      assert(await page.eval(`!document.querySelector(${lit(strip)})`));
      await noPartials('takeover-upload.bin');
    });
  });

  await check('Close the terminal and disconnect ends the shell and job and removes its notice', async () => {
    await disconnect(other);
    await kept(other, taker);
    await kept(page, taker);
    await returnTerminal(other);
    await disconnect(other, true);
    await processGone(shell);
    await processGone(job);
    for (const browser of [other, page]) {
      await browser.until(`!document.querySelector(${lit(keptNotice)}) && !d.devices().find((x) => x.id === ${lit(device)})?.kept_terminal`,
        'the detached notice cleared');
    }
  });

  await check('stopping the host ends a detached shell and its job', async () => {
    await openTerminal(other, device);
    await shellReady(other);
    const stoppedShell = await printedPid(other, 'STOPPID');
    const stoppedJob = await startJob(other);
    await disconnect(other);
    await kept(other, taker);
    await exec([...asHost, 'sh', '-ec', 'kill -TERM "$(cat "$1")"', 'stop', hostHome + '/.local/state/deskway/host.lock']);
    await waitFor(() => foreground.exited, 'the host to stop');
    await output(foreground, 'host stopped');
    await processGone(stoppedShell);
    await processGone(stoppedJob);
    await other.until(`!(${hostOnline}) && !document.querySelector(${lit(keptNotice)})`, 'the offline computer and cleared notice');
  });


  await check('the host output has file operations, device, paths and outcomes without file contents', async () => {
    for (const [op, path, outcome] of [
      ['list', hostHome, 'ok'],
      ['mkdir', hostHome + '/trial-created', 'ok'],
      ['rename', hostHome + '/trial-created', 'ok'],
      ['delete', hostHome + '/trial-renamed', 'not_empty'],
      ['delete', hostHome + '/trial-renamed', 'ok'],
      ['delete', hostHome + '/trial-link', 'ok'],
      ['list', hostHome + '/trial-unreadable', 'denied'],
    ]) assert(foreground.stderr.includes('File operation device="' + keeper + '" operation=' + op + ' path="' + path + '" outcome=' + outcome));
    assert(!foreground.stderr.includes(fileContents));
  });

  await check('the host records one line per transfer, with outcomes and without contents', async () => {
    const lines = foreground.stderr.split('\n').filter((line) => line.startsWith('File transfer device='));
    const expected = [
      ['upload', largeName, fileSize, 'done', ''],
      ...ordered.map((name) => ['upload', name, name.length, 'done', '']),
      ['upload', largeName, fileSize, 'cancelled', ''],
      ['upload', replacement, fileSize, 'done', '', true],
      ...ordered.map((name) => ['upload', name, name.length + 4, 'done', '', true]),
      ['upload', late, fileSize, 'cancelled', ''],
      ['upload', directoryConflict, 4, 'failed', 'not_regular'],
      ['upload', corrupt, fileSize, 'failed', 'integrity', true],
      ['upload', corrupt, fileSize, 'done', '', true],
      ['upload', cancelledName, fileSize, 'cancelled', ''],
      ['download', largeName, fileSize, 'done', ''],
      ['upload', resumedUpload, fileSize, 'done', ''],
      ['upload', queuedUpload, 'still queued'.length, 'done', ''],
      ['download', resumedDownload, fileSize, 'done', ''],
      ['upload', readmittedUpload, fileSize, 'done', ''],
      ['download', tooLarge, 64 * 1024 * 1024 + 1, 'failed', 'limit'],
      ['download', growing, fileSize, 'failed', 'changed'],
      ['upload', 'closed-upload.bin', fileSize, 'cancelled', ''],
      ['upload', 'takeover-upload.bin', fileSize, 'cancelled', ''],
    ];
    assert.equal(lines.length, expected.length);
    for (const [direction, name, size, outcome, why, replace = false] of expected) {
      const line = 'File transfer device="' + keeper + '" direction=' + direction +
        ' path="' + hostHome + '/' + name + '" size=' + size + ' outcome=' + outcome + ' why=' + why + ' replace=' + replace;
      assert.equal(lines.filter((actual) => actual.trimEnd() === line).length, 1, line);
    }
    assert(!foreground.stderr.includes(fileContents));
  });

  await check('the host output records terminal lifecycle events with devices and reasons', async () => {
    for (const line of [
      `Terminal opened by ${keeper}`,
      `Terminal left by ${keeper} (left)`,
      `Terminal left by ${keeper} (taken)`,
      `${taker} returned to the terminal`,
      'Terminal closed (shell exit status 17)',
      'Terminal closed (closed by the person)',
      'Terminal closed (host stopping)',
    ]) assert(foreground.stderr.includes(line), 'missing log entry: ' + line);
    assert.doesNotMatch(foreground.stderr, /LASTLINE|FLOODDONE|TRIAL_USER|COUNT\d+/);
  });
}

try {
  await run();
  await cleanup();
  if (!process.exitCode) record('PASS Linux browser terminal and files trial\n');
} catch (e) {
  record('FAIL Linux browser terminal and files trial\n' + (e.stack ?? e) + '\n');
  process.exitCode = 1;
  try { await cleanup(); } catch (error) { record('FAIL cleanup: ' + error + '\n'); }
}
