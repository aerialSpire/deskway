// A fresh account adds three Windows computers through the browser, the last on a later visit.
import { execFile, spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { promisify } from 'node:util';
import { SCREENS, TAKE_OVER, lit, q, issueAdminLink, openSetup, submitSetup, sleep, t } from './harness.mjs';

const runCommand = promisify(execFile);
const title = `document.querySelector('.dialog h2')?.textContent`;
const connected = `s?.info().state === 'active' && s.info().connection === 'connected' && s.info().input === 'open'`;
const user = 'first-use';
const password = 'first-use own password';
const digitsOnly = (code) => code.replace(/\D/g, '');

async function waitFor(read, label) {
  const until = Date.now() + 15000;
  while (Date.now() < until) {
    const value = await read();
    if (value) return value;
    await sleep(150);
  }
  throw new Error(`timed out waiting for ${label}`);
}

async function command(exe, args, env = process.env) {
  const { stdout } = await runCommand(exe, args, { env, encoding: 'utf8', windowsHide: true, timeout: 30000 });
  return stdout;
}

// Keep the first offer and its decision, seen by the browser. An answer for a later offer
// would not prove the first one was accepted.
async function observeOffers(p) {
  const byDevice = new Map(), bySession = new Map();
  p.ws.addEventListener('message', ({ data }) => {
    const { method, params } = JSON.parse(data);
    if (method !== 'Network.webSocketFrameSent' && method !== 'Network.webSocketFrameReceived') return;
    if (params.response.opcode !== 1) return;
    const { type, device, session, epoch, reason, devices } = JSON.parse(params.response.payloadData);
    if (method === 'Network.webSocketFrameSent' && type === 'offer' && !byDevice.has(device)) {
      const first = { id: session, epoch };
      byDevice.set(device, first);
      bySession.set(session, first);
    }
    if (method === 'Network.webSocketFrameReceived' && (type === 'answer' || type === 'bye')) {
      const first = bySession.get(session);
      if (first && (type === 'bye' || epoch === first.epoch)) first.reply ??= { type, reason };
    }
    if (method === 'Network.webSocketFrameReceived' && type === 'devices') {
      for (const device of devices ?? []) {
        const first = byDevice.get(device.id);
        if (first && device.controller?.device) first.admitted = true;
      }
    }
  });
  await p.send('Network.enable');
  return byDevice;
}

async function click(p, selector, label) {
  if (!await p.clickText(selector, label, { exact: true })) throw new Error(`could not click ${label}`);
}

async function openComputer(h, p, computer, id, offers, host) {
  await click(p, '.card-open', computer);
  await p.until(SCREENS, `${computer}’s screen button`);
  await p.eval(TAKE_OVER);
  if (await p.eval(`${title} === ${q('here.confirmTitle')}`)) {
    await click(p, '.dialog button', t('here.confirmGo'));
  }
  await p.until(`(${connected}) || !!(${title})`, `${computer}’s session to connect or end`, 35000);
  const first = offers.get(id);
  const requested = await p.eval(`${title} === ${q('notAdded.title', { name: computer })}`);
  const reply = first?.reply;
  const up = await p.v(`(${connected}) && s.info().id === ${JSON.stringify(first?.id ?? null)}`);
  h.check(`${computer} needs no Request control`, !!first && !requested);
  h.check(`${computer}’s first offer is admitted`, first?.admitted || reply?.type === 'answer', reply?.type === 'bye' && !first?.admitted ? reply.reason : '');
  h.check(`${computer} connects on the first offer`, reply?.type === 'answer' && up, up ? '' : host.log.trim());
  if (up) {
    await click(p, '.toolbar button', t('tb.disconnect'));
    await p.until(`${title} === ${q('leave.title', { name: computer })}`, 'the disconnect confirmation');
    await click(p, '.dialog button', t('tb.disconnect'));
    await p.until(`!document.querySelector('.session')`, 'the session to close');
  } else {
    await click(p, '.dialog button', t(requested ? 'common.cancel' : 'common.back'));
  }
}

async function turnOnAI(h, p, id, computer) {
  await p.eval(`document.querySelector('.name-btn').click()`);
  await click(p, '.menu:not([hidden]) .menu-item', t('devices.title'));
  const ai = `document.querySelector('[data-ai=${lit(id)}]')`;
  await p.until(`${ai}?.getAttribute('aria-checked') === 'false'`, 'the new computer’s AI switch to be off');
  await p.eval(`${ai}.click()`);
  await click(p, '.dialog button', t('devices.allow'));
  const checked = `${ai}?.getAttribute('aria-checked') === 'true'`;
  await p.until(`${checked} || ${title} === ${q('devices.notChanged')} || !!document.querySelector('[data-ai-refused=${lit(id)}]')`, 'the computer to accept or refuse AI');
  h.check(`${computer} accepts the AI setting`, await p.eval(checked));
  if (await p.eval(`${title} === ${q('devices.notChanged')}`)) await click(p, '.dialog button', t('common.ok'));
}

export const area = {
  name: 'first-use',
  needs: [],
  async run(h) {
    const { url } = h.opts;
    const address = new URL(url);
    if (process.platform !== 'win32' || address.hostname !== 'localhost' || address.protocol !== 'http:' || !address.port || !h.opts.serverExe || !h.opts.hostExe) {
      throw new Error('first-use needs Windows, --url http://localhost:PORT, --server-exe and --host-exe');
    }
    const serverExe = resolve(h.opts.serverExe), hostExe = resolve(h.opts.hostExe);
    const dir = mkdtempSync(join(tmpdir(), 'deskway-first-use-'));
    const stateFile = join(dir, 'state.json');
    const children = [], panels = [];
    const start = (exe, args, env = process.env) => {
      const proc = spawn(exe, args, { env, windowsHide: true, stdio: ['ignore', 'ignore', 'pipe'] });
      const child = { proc, log: '', closed: new Promise((done) => proc.once('close', done)) };
      children.push(child);
      proc.stderr.setEncoding('utf8').on('data', (line) => { child.log += line; });
      proc.on('error', (err) => { child.log += String(err); });
      return child;
    };

    try {
      const server = start(serverExe, ['-listen', `127.0.0.1:${address.port}`, '-data', stateFile]);
      await waitFor(async () => server.log.includes('listening on') && (await fetch(url, { signal: AbortSignal.timeout(1000) }).catch(() => null))?.ok, 'the fresh server');

      let p = await h.newPage();
      let offers = await observeOffers(p);
      await openSetup(p, await issueAdminLink(serverExe, stateFile, url));
      h.check('the setup link opens in a browser with no device key', await p.eval(`!localStorage.getItem('deskway.passkey')`));
      await p.eval(`(() => {
        const fetch = window.fetch;
        window.fetch = (path, options) => {
          if (path === '/api/setup' && options?.method === 'POST') {
            window.__setupCred = JSON.parse(localStorage.getItem('deskway.passkey'))?.cred;
            window.fetch = fetch;
          }
          return fetch(path, options);
        };
      })()`);
      await submitSetup(p, { user, password });
      await p.until(`!!document.querySelector('.page .name-btn')`, 'signed in through the account link');
      h.check('the key prepared before redemption is registered on the account', await p.eval(`(async () => {
        const devices = await (await fetch('/api/devices')).json();
        return !!window.__setupCred && devices.find((d) => d.current)?.device?.cred === window.__setupCred;
      })()`));

      for (let i = 1; i <= 3; i++) {
        if (i === 3) {
          await h.closePage(p);
          p = await h.newPage();
          offers = await observeOffers(p);
          await p.until(`document.querySelectorAll('.card-open').length === 2`, 'both existing computers after reopening the page');
        }

        const computer = `Computer ${i}`;
        const home = join(dir, `host-${i}`);
        const env = { ...process.env, DESKWAY_HOME: home };
        await command(hostExe, ['enroll', '--server', url, '--user', user, '--name', computer], { ...env, DESKWAY_PASSWORD: password });
        const { id } = JSON.parse(readFileSync(join(home, 'host.json'), 'utf8'));
        const host = start(hostExe, ['net', '--dev'], env);
        start(hostExe, ['agent', '--dev'], env);
        await waitFor(() => host.log.includes('Deskway host:'), `${computer} to start`);
        let panel;
        const approval = await waitFor(() => {
          if (!panel || panel.error) {
            panel?.close();
            panel = h.panelPipe(home);
            panels.push(panel);
          }
          return panel.statuses.find((status) => status.waiting && status.approval_code);
        }, `${computer}’s panel approval code`);
        await p.until(`${title} === ${q('approve.computerTitle')}`, 'computer approval');
        const approvalText = await p.eval(`document.querySelector('.dialog').textContent`);
        if (!approvalText.includes(computer) || !digitsOnly(approvalText).includes(digitsOnly(approval.approval_code))) {
          throw new Error(`browser approval does not match ${computer} and its panel code`);
        }
        await click(p, '.dialog button', t('common.approve'));
        await p.until(`d?.devices().some((d) => d.id === ${lit(id)} && d.online) && [...document.querySelectorAll('.card-open')].some((b) => b.textContent === ${lit(computer)})`, `${computer} online on the list`);

        await openComputer(h, p, computer, id, offers, host);
        await turnOnAI(h, p, id, computer);
        await p.until(`!!document.querySelector('.title-back')`, 'Back to My computers');
        await p.eval(`document.querySelector('.title-back').click()`);
      }
    } finally {
      for (const panel of panels) panel.close();
      for (const child of children.reverse()) child.proc.kill();
      await Promise.all(children.map(({ closed }) => closed));
      rmSync(dir, { recursive: true, force: true });
    }
  },
};
