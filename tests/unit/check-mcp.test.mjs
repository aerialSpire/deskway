import assert from 'node:assert/strict';
import test from 'node:test';
import { mcpClient } from '../../scripts/check-mcp.mjs';

test('the MCP check client skips an empty SSE priming event', async (t) => {
  const result = { tools: [] };
  t.mock.method(globalThis, 'fetch', async (_url, { body }) => {
    const reply = JSON.stringify({ jsonrpc: '2.0', id: JSON.parse(body).id, result });
    return new Response('id: prime\ndata:\n\ndata: ' + reply + '\n\n', {
      headers: { 'Content-Type': 'text/event-stream' },
    });
  });
  const client = mcpClient({ resource: 'https://desk.example.net/mcp' }, 'test-token');
  assert.deepEqual(await client.rpc('tools/list'), result);
});
