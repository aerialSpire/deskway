import assert from 'node:assert/strict';
import test from 'node:test';
import { aiDeviceTag, mcpDecision, mcpRequest } from '../../web/static/js/mcp-approval.js';

test('only the OAuth approval page uses the request handle', () => {
  for (const [path, request] of [['/?mcp=a', 'a'], ['/?request=a', null], ['/oauth/authorize?mcp=a', null]]) {
    assert.equal(mcpRequest(new URL(path, 'https://desk.example')), request);
  }
});

test('device tags distinguish a remote application from a local AI client', () => {
  for (const [row, tag] of [
    [{ device: { role: 'ai' }, remote_mcp: true }, 'devices.tagRemoteMcp'],
    [{ device: { role: 'ai' } }, 'devices.tagAI'],
    [{ device: {} }, null],
  ]) assert.equal(aiDeviceTag(row), tag);
});

test('approval signs the prepared device and denial needs no signature', async () => {
  const device = { cred: 'application', role: 'ai' };
  const proof = { by: 'browser', ts: 123, sig: 'signature' };
  for (const approve of [true, false]) {
    const calls = [];
    const redirect = approve ? 'https://app.example/?code=1' : 'https://app.example/?error=access_denied';
    assert.equal(await mcpDecision('request-id', approve, async (body) => {
      calls.push(body);
      const result = body.action === 'prepare' ? { device, endorsed: 122 } : { redirect };
      return { ok: true, json: async () => result };
    }, async (actual, least) => {
      assert(approve);
      assert.equal(least, 123);
      assert.equal(actual, device);
      return proof;
    }), redirect);
    assert.deepEqual(calls.map(({ request, action, proof }) => [request, action, proof]),
      approve ? [['request-id', 'prepare', undefined], ['request-id', 'approve', proof]] : [['request-id', 'cancel', undefined]]);
  }
});

test('a failed preparation never signs or approves', async () => {
  await assert.rejects(mcpDecision('request-id', true, async ({ action }) => {
    assert.equal(action, 'prepare');
    return { ok: false, json: async () => ({ error: 'grant_limit' }) };
  }, async () => assert.fail('signature requested')), /grant_limit/);
});
