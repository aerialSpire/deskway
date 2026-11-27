import assert from 'node:assert/strict';
import { test } from 'node:test';
import { browserSignIn } from '../../web/static/js/browser-sign-in.js';

test('the signed-in page waits for its device key to be reported', async () => {
  const response = { status: 200, ok: true };
  const key = { key: 'public' };
  const { promise: started, resolve: reporting } = Promise.withResolvers();
  const { promise: reported, resolve: finish } = Promise.withResolvers();
  const signIn = browserSignIn({
    getSession: async () => response,
    ensurePasskey: async () => key,
    reportPasskey: (rec) => { reporting(rec); return reported; },
  });
  let ready = false;
  const current = signIn.current().then((res) => { ready = true; return res; });
  assert.equal(await started, key);
  assert.equal(ready, false);
  finish(true);
  assert.equal(await current, response);
});

test('a signed-out browser does not prepare a key', async () => {
  const response = { status: 401, ok: false };
  const signIn = browserSignIn({ getSession: async () => response });
  assert.equal(await signIn.current(), response);
});

test('a waiting browser reports the named key before showing its code', async () => {
  const key = { key: 'public' };
  const steps = [];
  const signIn = browserSignIn({
    ensurePasskey: async (name) => { steps.push(name); return key; },
    reportPasskey: async (rec) => { steps.push(rec); return true; },
    keyCode: async (pub) => { steps.push(pub); return '123456'; },
  });
  assert.deepEqual(await signIn.waiting('New device', '000000'), { code: '123456', noKey: false });
  assert.deepEqual(steps, ['New device', key, key.key]);
});

test('an unreported key cannot supply the waiting browser’s code', async () => {
  const signIn = browserSignIn({
    ensurePasskey: async () => ({ key: 'public' }),
    reportPasskey: async () => false,
  });
  assert.deepEqual(await signIn.waiting('New device', '000000'), { code: '000000', noKey: true });
});
