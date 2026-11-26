import { createHash, randomBytes } from 'node:crypto';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { createInterface } from 'node:readline';
import { Readable } from 'node:stream';
import { setTimeout as delay } from 'node:timers/promises';
import { pathToFileURL } from 'node:url';

const clientInfo = { name: 'Deskway MCP check', version: '1' };
const versions = ['2025-11-25', '2025-06-18'];
const accept = 'application/json, text/event-stream';
const callbackPath = '/callback';

class CheckError extends Error {}

function ensure(value, message) {
  if (!value) throw new CheckError(message);
}

function httpsURL(value) {
  const url = URL.canParse(value) ? new URL(value) : null;
  ensure(url?.protocol === 'https:' && !url.username && !url.password && !url.hash, 'Expected an HTTPS URL');
  return url;
}

async function request(url, options) {
  try {
    return await fetch(url, { redirect: 'error', signal: AbortSignal.timeout(250000), ...options });
  } catch {
    throw new CheckError('HTTPS request failed; check the URL, TLS trust and server availability');
  }
}

async function json(response, status = 200) {
  ensure(response.status === status, `HTTP ${response.status}, expected ${status}`);
  return response.json();
}

async function step(log, name, run) {
  try {
    const result = await run();
    log('PASS ' + name);
    return result;
  } catch (error) {
    const reason = error instanceof CheckError ? error.message : 'Invalid response or I/O failure';
    throw new CheckError(name + ' — ' + reason);
  }
}

export async function discover(endpoint, log = () => {}) {
  const resource = await step(log, 'MCP resource URI', () => httpsURL(endpoint));
  const metadataURL = await step(log, 'Bearer challenge (WWW-Authenticate)', async () => {
    const response = await request(resource, { headers: { Accept: accept } });
    ensure(response.status === 401, `HTTP ${response.status}, expected 401`);
    const challenge = response.headers.get('www-authenticate') ?? '';
    await response.body?.cancel();
    const bearer = /(?:^|,\s*)Bearer\s+(.*?)(?=,\s*[\w-]+\s+(?![ \t]*=)|$)/i.exec(challenge)?.[1];
    const metadata = /(?:^|,)\s*resource_metadata\s*=\s*(?:"([^"]+)"|([^,\s]+))/i.exec(bearer ?? '');
    ensure(metadata, 'Missing Bearer resource_metadata');
    return httpsURL(metadata[1] ?? metadata[2]);
  });
  const issuer = await step(log, 'OAuth Protected Resource Metadata', async () => {
    const metadata = await json(await request(metadataURL));
    ensure(httpsURL(metadata.resource).href === resource.href, 'Resource identifier differs from the MCP address');
    return httpsURL(metadata.authorization_servers?.[0]);
  });
  const metadata = await step(log, 'OAuth Authorization Server Metadata', async () => {
    const url = new URL('/.well-known/oauth-authorization-server' + issuer.pathname.replace(/\/$/, ''), issuer);
    const data = await json(await request(url));
    ensure(httpsURL(data.issuer).href === issuer.href, 'Issuer mismatch');
    ensure(data.code_challenge_methods_supported?.includes('S256'), 'PKCE S256 is unavailable');
    return data;
  });
  return { resource, metadata };
}

export async function register({ metadata }, name, redirect) {
  const registration = await json(await request(httpsURL(metadata.registration_endpoint), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      client_name: name, redirect_uris: [redirect], response_types: ['code'],
      grant_types: ['authorization_code', 'refresh_token'], token_endpoint_auth_method: 'none',
    }),
  }), 201);
  ensure(typeof registration.client_id === 'string' && registration.client_id, 'Missing client_id');
  return registration.client_id;
}

export function authorizationRequest({ resource, metadata }, clientID, redirect) {
  const url = httpsURL(metadata.authorization_endpoint);
  const verifier = randomBytes(32).toString('base64url');
  const state = randomBytes(24).toString('hex');
  for (const [key, value] of Object.entries({
    client_id: clientID, redirect_uri: redirect, response_type: 'code', state,
    resource: resource.href, code_challenge_method: 'S256',
    code_challenge: createHash('sha256').update(verifier).digest('base64url'),
  })) url.searchParams.set(key, value);
  return { url, state, verifier };
}

async function exchange({ resource, metadata }, clientID, fields) {
  return request(httpsURL(metadata.token_endpoint), {
    method: 'POST',
    body: new URLSearchParams({ client_id: clientID, resource: resource.href, ...fields }),
  });
}

async function tokens(response) {
  const result = await json(response);
  ensure(typeof result.access_token === 'string' && result.access_token && /^Bearer$/i.test(result.token_type),
    'Missing Bearer access token');
  return result;
}

export async function authorize(server, { clientID, name = clientInfo.name, open, log = () => {} }) {
  let receive, state;
  const reply = new Promise((resolve) => { receive = resolve; });
  const listener = createServer((req, res) => {
    const params = req.url?.startsWith(callbackPath + '?') ? new URLSearchParams(req.url.slice(callbackPath.length)) : null;
    if (!state || req.method !== 'GET' || params?.get('state') !== state) {
      res.writeHead(400).end('Invalid OAuth callback.');
      return;
    }
    res.writeHead(200, { 'Content-Type': 'text/plain', 'Cache-Control': 'no-store' }).end('Return to the terminal for the MCP check result.');
    receive(params);
  });
  const timer = new AbortController();
  try {
    const redirect = await step(log, 'OAuth loopback redirect', async () => {
      listener.listen(0, '127.0.0.1');
      await once(listener, 'listening');
      return `http://127.0.0.1:${listener.address().port}${callbackPath}`;
    });
    clientID ??= await step(log, 'OAuth Dynamic Client Registration', () => register(server, name, redirect));
    const code = await step(log, 'OAuth authorization code', async () => {
      const pending = authorizationRequest(server, clientID, redirect);
      state = pending.state;
      const [params] = await Promise.race([
        Promise.all([reply, Promise.resolve().then(() => open(pending.url))]),
        delay(300000, null, { signal: timer.signal }).then(() => { throw new CheckError('Timed out waiting for the loopback redirect'); }),
      ]);
      const issuer = params.get('iss');
      ensure(issuer === server.metadata.issuer || (!issuer && !server.metadata.authorization_response_iss_parameter_supported), 'Issuer mismatch');
      if (params.get('error') === 'access_denied' && !params.has('code')) throw new CheckError('access_denied');
      ensure(params.get('code') && !params.has('error'), 'Missing authorization code');
      return { code: params.get('code'), code_verifier: pending.verifier };
    });
    const credentials = await step(log, 'OAuth token endpoint (PKCE)', async () => tokens(await exchange(server, clientID, {
      grant_type: 'authorization_code', redirect_uri: redirect, ...code,
    })));
    return { clientID, credentials };
  } finally {
    timer.abort();
    listener.close();
    listener.closeAllConnections();
  }
}

export async function refresh(server, clientID, refreshToken) {
  return tokens(await exchange(server, clientID, { grant_type: 'refresh_token', refresh_token: refreshToken }));
}

async function rpcResponse(response, id) {
  const type = response.headers.get('content-type')?.split(';', 1)[0];
  if (type === 'text/event-stream') {
    const stream = Readable.fromWeb(response.body);
    let data = [];
    try {
      for await (const line of createInterface({ input: stream, crlfDelay: Infinity })) {
        if (line.startsWith('data:')) data.push(line.slice(5).replace(/^ /, ''));
        if (line === '' && data.length) {
          const payload = data.join('\n');
          data = [];
          if (payload === '') continue;
          const message = JSON.parse(payload);
          if (message.id === id) return message;
        }
      }
    } finally {
      stream.destroy();
    }
    throw new CheckError('SSE stream ended before the JSON-RPC response');
  }
  ensure(type === 'application/json', 'Expected JSON or an SSE stream');
  return response.json();
}

export function mcpClient({ resource }, accessToken) {
  let id = 0;
  let version = versions[0];
  let session;
  const send = (message) => request(resource, {
    method: 'POST',
    headers: {
      Accept: accept, 'Content-Type': 'application/json', Authorization: 'Bearer ' + accessToken,
      ...(message.method === 'initialize' ? {} : { 'MCP-Protocol-Version': version }),
      ...(session ? { 'Mcp-Session-Id': session } : {}),
    },
    body: JSON.stringify({ jsonrpc: '2.0', ...message }),
  });
  async function rpc(method, params = {}, status = 200) {
    const requestID = ++id;
    const response = await send({ method, params, id: requestID });
    ensure(response.status === status, `HTTP ${response.status}, expected ${status}`);
    if (status !== 200) {
      await response.body?.cancel();
      return;
    }
    session ??= response.headers.get('mcp-session-id');
    const reply = await rpcResponse(response, requestID);
    ensure(reply.jsonrpc === '2.0' && reply.id === requestID, 'Invalid JSON-RPC response');
    ensure(Object.hasOwn(reply, 'result') && !reply.error, 'JSON-RPC request failed');
    return reply.result;
  }
  return {
    rpc,
    async initialize(status = 200) {
      const result = await rpc('initialize', { protocolVersion: version, clientInfo, capabilities: {} }, status);
      if (status !== 200) return;
      ensure(versions.includes(result.protocolVersion), 'Unsupported protocolVersion');
      version = result.protocolVersion;
      const response = await send({ method: 'notifications/initialized' });
      await response.body?.cancel();
      ensure(response.status === 202, `notifications/initialized: HTTP ${response.status}, expected 202`);
    },
    async tool(name, args = {}) {
      const result = await rpc('tools/call', { name, arguments: args });
      ensure(!result.isError && result.structuredContent, 'MCP tool returned an error or no structuredContent');
      return result.structuredContent;
    },
  };
}

export async function checkMCP(endpoint, { checkRefresh = false, log = console.log, open = () => {} } = {}) {
  const server = await discover(endpoint, log);
  const approval = await authorize(server, { log, open: (url) => {
    log('Authorization URL: ' + url.href);
    return open(url);
  } });
  const { clientID } = approval;
  let { credentials } = approval;
  try {
    const client = mcpClient(server, credentials.access_token);
    await step(log, 'MCP initialize and notifications/initialized', () => client.initialize());
    const tools = await step(log, 'MCP tools/list', async () => {
      const { tools } = await client.rpc('tools/list');
      ensure(Array.isArray(tools) && tools.some(({ name }) => name === 'computers'), 'Missing computers tool');
      return tools;
    });
    log('Tools: ' + tools.map(({ name }) => name).join(', '));
    const computers = await step(log, 'MCP tools/call (computers)', async () => {
      const { result } = await client.tool('computers');
      ensure(Array.isArray(result), 'Expected a computer list');
      return result;
    });
    log('Computers: ' + (computers.map(({ name }) => JSON.stringify(name)).join(', ') || 'none'));
    if (checkRefresh) {
      const old = credentials.refresh_token;
      credentials = await step(log, 'OAuth refresh token grant', async () => {
        ensure(typeof old === 'string' && old, 'Missing refresh token');
        const next = await refresh(server, clientID, old);
        ensure(typeof next.refresh_token === 'string' && next.refresh_token && next.refresh_token !== old, 'Refresh token did not rotate');
        const refreshed = mcpClient(server, next.access_token);
        await refreshed.initialize();
        return next;
      });
      await step(log, 'OAuth refresh token reuse (invalid_grant)', async () => {
        const result = await json(await exchange(server, clientID, { grant_type: 'refresh_token', refresh_token: old }), 400);
        ensure(result.error === 'invalid_grant', 'Expected invalid_grant for the used refresh token');
      });
    }
    return { server, clientID, name: clientInfo.name, credentials, computers };
  } finally {
    log(`${clientInfo.name} is in My devices. You can remove it there.`);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  const checkRefresh = args.includes('--refresh');
  const addresses = args.filter((arg) => arg !== '--refresh');
  if (addresses.length !== 1 || addresses[0].startsWith('-')) {
    console.error('Usage: node scripts/check-mcp.mjs https://desk.example.net/mcp [--refresh]');
    process.exitCode = 2;
  } else {
    try {
      await checkMCP(addresses[0], { checkRefresh });
    } catch (error) {
      console.error('FAIL ' + (error instanceof CheckError ? error.message : 'MCP check — Invalid response or I/O failure'));
      process.exitCode = 1;
    }
  }
}
