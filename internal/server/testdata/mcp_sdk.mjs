import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { endorseChallenge } from '../../../web/static/js/auth-messages.js';

const [sdk, issuer] = process.argv.slice(2);
const load = (path) => import(pathToFileURL(join(sdk, 'dist/esm/client', path)).href);
const { Client } = await load('index.js');
const { StreamableHTTPClientTransport } = await load('streamableHttp.js');
const { UnauthorizedError } = await load('auth.js');

const redirectUrl = 'http://127.0.0.1:3000/callback';
const requestState = randomBytes(16).toString('hex');
const browserHeaders = { 'Content-Type': 'application/json', Cookie: process.env.DESKWAY_MCP_TEST_COOKIE, Origin: issuer };
const browserKey = await crypto.subtle.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, true, ['sign', 'verify']);
const cred = randomBytes(16).toString('base64url');
const key = Buffer.from(await crypto.subtle.exportKey('spki', browserKey.publicKey)).toString('base64');
const post = (path, body) => fetch(issuer + path, { method: 'POST', headers: browserHeaders, body: JSON.stringify(body) });
assert.equal((await post('/api/login/device', { cred, alg: -7, key, name: 'Test browser' })).status, 200);

let registration, tokens, verifier, code;
const provider = {
  redirectUrl,
  clientMetadata: {
    client_name: 'SDK trial application',
    redirect_uris: [redirectUrl],
    token_endpoint_auth_method: 'none',
    grant_types: ['authorization_code', 'refresh_token'],
  },
  state: () => requestState,
  clientInformation: () => registration,
  saveClientInformation: (value) => { registration = value; },
  tokens: () => tokens,
  saveTokens: (value) => { tokens = value; },
  codeVerifier: () => verifier,
  saveCodeVerifier: (value) => { verifier = value; },
  async redirectToAuthorization(url) {
    const auth = await fetch(url, { redirect: 'manual' });
    assert.equal(auth.status, 303);
    const root = new URL(auth.headers.get('location'), issuer);
    assert.equal(root.pathname, '/');
    const request = root.searchParams.get('mcp');
    assert(request);
    assert.equal((await fetch(issuer + '/api/mcp/approval?' + new URLSearchParams({ request }), { headers: browserHeaders })).status, 200);
    const prepared = await post('/api/mcp/approval', { request, action: 'prepare' });
    assert.equal(prepared.status, 200);
    const { device, endorsed } = await prepared.json();
    const ts = Math.max(Math.floor(Date.now() / 1000), endorsed + 1);
    const challenge = createHash('sha256').update(endorseChallenge(device, ts)).digest();
    const sig = Buffer.from(await crypto.subtle.sign({ name: 'ECDSA', hash: 'SHA-256' }, browserKey.privateKey, challenge)).toString('base64url');
    const approval = await post('/api/mcp/approval', { request, action: 'approve', proof: { ts, by: cred, sig } });
    assert.equal(approval.status, 200);
    const callback = new URL((await approval.json()).redirect);
    assert.equal(callback.searchParams.get('iss'), issuer);
    assert.equal(callback.searchParams.get('state'), requestState);
    code = callback.searchParams.get('code');
    assert(code);
  },
};
const endpoint = new URL('/mcp', issuer);
const transport = () => new StreamableHTTPClientTransport(endpoint, { authProvider: provider });
const first = transport();
const client = new Client({ name: 'SDK trial', version: '1' });
await assert.rejects(client.connect(first), UnauthorizedError);
await first.finishAuth(code);
await client.connect(transport());
try {
  const result = await client.callTool({ name: 'test', arguments: {} });
  assert.deepEqual(result.content, [{ type: 'text', text: 'hello from Deskway' }]);
  console.log('PASS SDK discovery, dynamic registration, signed authorization, token exchange and tool call');
} finally {
  await client.close();
}
