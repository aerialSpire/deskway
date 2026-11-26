import { $app, h, loading } from '../dom.js';
import { t } from '../i18n.js';
import { hasDeviceKey, signWith } from '../auth.js';
import { endorseChallenge } from '../auth-messages.js';
import { state } from '../state.js';
import { mcpRequest, mcpDecision } from '../mcp-approval.js';
import { heading, topbar } from './common.js';

const endpoint = '/api/mcp/approval';

function answer(request, approve) {
  return mcpDecision(request, approve, (body) => fetch(endpoint, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  }), (device, least) => signWith((ts) => endorseChallenge(device, ts), least));
}

export function mcpSignInNotice() {
  return mcpRequest(location) ? h('p', { class: 'hint' }, t('mcp.trusted')) : null;
}

let drawing = 0;

export async function renderMcpApproval() {
  const mine = ++drawing;
  const request = mcpRequest(location);
  const page = (...children) => $app.replaceChildren(h('div', { class: 'page single', 'data-page': 'mcp-approval' },
    topbar(), heading(t('mcp.title')), h('div', { class: 'form' }, children)));
  page(loading(t('common.loading')));
  try {
    const [res, key] = await Promise.all([fetch(endpoint + '?' + new URLSearchParams({ request })), hasDeviceKey()]);
    if (!res.ok) throw new Error('authorization request');
    const app = await res.json();
    if (mine !== drawing || state.view !== 'mcp-approval') return;
    const error = h('div', { class: 'error', role: 'alert' });
    const decide = (approve) => async () => {
      accept.disabled = cancel.disabled = true;
      error.textContent = '';
      try { location.assign(await answer(request, approve)); }
      catch (e) {
        const message = {
          restart_required: 'mcp.expired', grant_limit: 'mcp.grantLimit', family_limit: 'mcp.familyLimit',
        }[e.message];
        error.textContent = t(message || 'mcp.failed');
        accept.disabled = cancel.disabled = !!message;
      }
    };
    const canApprove = app.canApprove && key;
    const accept = h('button', { class: 'btn primary block', type: 'button', disabled: !canApprove, onclick: decide(true) }, t('common.approve'));
    const cancel = h('button', { class: 'btn text', type: 'button', disabled: !canApprove, onclick: decide(false) }, t('common.cancel'));
    page(h('p', { class: 'hint' }, t('mcp.supplied')),
      h('dl', {},
        h('dt', {}, t('mcp.name')), h('dd', {}, h('bdi', {}, app.name)),
        h('dt', {}, t('mcp.redirect')), h('dd', {}, h('bdi', {}, app.redirectHost)),
        app.clientHost ? h('dt', {}, t('mcp.clientHost')) : null,
        app.clientHost ? h('dd', {}, h('bdi', {}, app.clientHost)) : null),
      h('p', {}, t('mcp.access')),
      app.loopback ? h('p', { class: 'hint' }, t('mcp.loopback')) : null,
      canApprove ? null : h('p', { class: 'hint' }, t('mcp.trusted')),
      error, accept, cancel);
  } catch {
    if (mine === drawing && state.view === 'mcp-approval') page(h('p', { class: 'hint' }, t('mcp.expired')));
  }
}
