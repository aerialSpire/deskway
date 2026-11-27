// End-to-end tests: drives a headless browser against a running server and host over the
// Chrome DevTools Protocol. Runs every area (tests/e2e/*.test.mjs) in order, or the ones named
// with --areas and those they need. Reports video metrics only; it never saves screen frames.
//
//   node tests/e2e/run.mjs --browser PATH --url URL --user NAME --password-file FILE
//     --host-exe PATH --home DIR --out DIR
//     [--devtools-port 9333] [--areas auth,session,...] [--server-exe PATH --server-state FILE]
// --user and --password-file identify an account with a password and an enrolled host.
// Auth and devices issue admin recovery links using --server-exe and --server-state.
// To run auth with a member account or without local access to the server, pass a fresh reset
// link in --setup-link-file. The relay area also uses --setup-link-file.
// URL must use a host name such as localhost: device keys and passkeys need a secure context.
// --host-exe and --home let the tests open pairing windows on the test host, as its owner would.
// On macOS, --areas host-restart starts and restarts a host with that separate test profile.
// --server-exe and --server-state add the check that a server restart keeps the session.
// --areas previous-protocol uses a fresh account with a previous-version host.
// --areas vitals uses a desktop host with workspace access and saves list and detail screenshots.
// Run --areas scan-open against fresh public-mode and local-mode test servers, each with its own
// --url, --server-exe and --server-state. Use a local-mode server with a private IPv4 address.
// --areas relay --relay-tls uses only TURN over the HTTPS port. Start the host with
// DESKWAY_TEST_RELAY=tls and give --url the HTTPS address advertised for TURN.
// Silently drop the host's UDP and TCP traffic to the server's usual TURN port.
// Several runs can go side by side with their own --devtools-port and server --url.
// On Windows, run --areas first-use alone. It starts its own server and three hosts with fresh state.
// It needs --browser, --url http://localhost:PORT, --server-exe and --host-exe.
// On Windows, --areas windows-workspace uses the same executable and localhost options as
// first-use. It runs under the signed-in, unlocked console account.
// --browser-arg=FLAG forwards a browser startup flag. It can be repeated.
import { Harness, options } from './harness.mjs';

// In the order they run. Every area but panel needs auth, which signs in `h.a` (the first
// device) and finds `h.hostId` (the test computer); the others share nothing else they rely on.
// An area that changes the account or the browser puts it back before it ends (devices: the
// passwords and names; i18n and layout: the language). `manage` removes the test computer from
// the account, so it runs last.
const AREAS = ['auth', 'session', 'transfer', 'clipboard', 'keyboard', 'input', 'takeover', 'lifecycle', 'devices', 'members', 'touch', 'security', 'i18n', 'layout', 'panel', 'pwa', 'ai', 'manage'];

// Run only when named. clipboard-host uses the real clipboard of the computer running the
// tests; relay needs a fresh member account and a host started for it; first-use runs its
// own local server and Windows hosts.
const OPT_IN = ['clipboard-host', 'relay', 'host-restart', 'first-use', 'previous-protocol', 'windows-workspace', 'vitals', 'scan-open'];
const opts = options();
const modules = Object.fromEntries(await Promise.all([...AREAS, ...OPT_IN].map(async (name) => [name, (await import(`./${name}.test.mjs`)).area])));
const wanted = new Set();
const add = (name) => {
  if (!modules[name]) throw new Error(`no area "${name}" (areas: ${[...AREAS, ...OPT_IN].join(', ')})`);
  if (wanted.has(name)) return;
  for (const n of modules[name].needs || []) add(n);
  wanted.add(name);
};
for (const name of opts.areas || AREAS) add(name);

const h = new Harness(opts);
let code = 1;
try {
  await h.launch();
  if (wanted.has('host-restart')) h.startTestHost();
  for (const name of [...AREAS, ...OPT_IN].filter((n) => wanted.has(n))) {
    console.log(`---- ${name}`);
    await modules[name].run(h);
  }
  const errors = h.pages.flatMap((p) => p.errors);
  h.check('no uncaught page errors', errors.length === 0, errors.join(' | '));
  code = h.results.every((r) => r.ok) ? 0 : 1;
} catch (e) {
  console.log(`FAIL  ${e.message}`);
} finally {
  console.log(`\n${h.results.filter((r) => r.ok).length} PASS / ${h.results.filter((r) => !r.ok).length + (code && h.results.every((r) => r.ok) ? 1 : 0)} FAIL`);
  await h.shutdown();
  process.exit(code);
}
