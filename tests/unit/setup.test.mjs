import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { test } from 'node:test';

globalThis.document = { getElementById: () => null, addEventListener() {} };
globalThis.matchMedia = () => ({ matches: false });

const { captureSetupLink } = await import('../../web/static/js/views/setup.js');

const code = 'a'.repeat(43);
const fields = { password: 'my new password' };
const device = { cred: 'credential', alg: -7, key: 'public', name: 'Test browser' };

function openLink(prepare = async () => device) {
  globalThis.location = new URL('https://desk.example/setup#c=' + code);
  globalThis.history = {
    state: null,
    replaceState(_state, _title, path) { globalThis.location = new URL(path, location); },
  };
  return captureSetupLink(prepare);
}

test('the setup view strips the fragment before any request and sends the code once', async (t) => {
  const requests = [];
  const { promise: reply, resolve: finish } = Promise.withResolvers();
  t.mock.method(globalThis, 'fetch', async (path, options) => {
    assert.equal(location.hash, '');
    requests.push({ path, ...options });
    if (options.method === 'GET') return Response.json({ kind: 'reset', name: 'friend' });
    return reply;
  });

  const link = openLink();
  assert.equal(location.href, 'https://desk.example/setup');
  assert.equal(requests.length, 0);
  assert.deepEqual(await (await link.details()).json(), { kind: 'reset', name: 'friend' });
  assert.equal(requests[0].path, '/api/setup?c=' + createHash('sha256').update(code).digest('hex'));
  assert.ok(!JSON.stringify(requests[0]).includes(code));
  const redeeming = link.redeem(fields);
  assert.equal(await link.redeem(fields), undefined);
  finish(new Response(null, { status: 204 }));
  assert.equal((await redeeming).status, 204);
  assert.equal(await link.redeem(fields), undefined);
  assert.equal(requests.length, 2);
  assert.equal(requests[1].path, '/api/setup');
  assert.equal(requests[1].method, 'POST');
  assert.deepEqual(JSON.parse(requests[1].body), { code, ...fields, device });
});

test('setup prepares the browser key before posting', async (t) => {
  const steps = [];
  const { promise: ready, resolve: finish } = Promise.withResolvers();
  t.mock.method(globalThis, 'fetch', async (path, { method, body }) => {
    steps.push(`${method} ${path}`);
    assert.deepEqual(JSON.parse(body).device, device);
    return new Response(null, { status: 204 });
  });

  const link = openLink(() => { steps.push('prepare'); return ready; });
  const redeeming = link.redeem(fields);
  assert.deepEqual(steps, ['prepare']);
  finish(device);
  assert.equal((await redeeming).status, 204);
  assert.deepEqual(steps, ['prepare', 'POST /api/setup']);
});

test('setup does not post when the browser cannot keep a key', async (t) => {
  const fetch = t.mock.method(globalThis, 'fetch', async () => new Response(null, { status: 204 }));
  let attempts = 0;
  const link = openLink(async () => {
    if (++attempts === 1) throw new Error('nokey');
    return device;
  });

  assert.equal((await link.redeem(fields)).status, 422);
  assert.equal(fetch.mock.callCount(), 0);
  assert.equal((await link.redeem(fields)).status, 204);
});

