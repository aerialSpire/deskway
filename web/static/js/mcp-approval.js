export function mcpRequest({ pathname, search }) {
  return pathname === '/' ? new URLSearchParams(search).get('mcp') : null;
}

export function aiDeviceTag(row) {
  if (row.remote_mcp) return 'devices.tagRemoteMcp';
  return row.device?.role === 'ai' ? 'devices.tagAI' : null;
}

export async function mcpDecision(request, approve, send, endorse) {
  const answer = async (action, proof) => {
    const res = await send({ request, action, proof });
    if (!res.ok) {
      const result = await res.json().catch(() => ({}));
      throw new Error(result.error || 'MCP approval failed');
    }
    return res.json();
  };
  if (!approve) return (await answer('cancel')).redirect;
  const { device, endorsed } = await answer('prepare');
  return (await answer('approve', await endorse(device, endorsed + 1))).redirect;
}
