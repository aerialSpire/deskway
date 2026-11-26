// Wire messages of every channel the browser speaks, written by hand from proto/messages.json
// and checked against it by tests/contract/proto.test.mjs.
//
// Field specs are short strings: s string, i integer, n number, b boolean, o object, a any,
// [T] array of T, {T} map of T, a type name, s:x|y a string limited to those values; a
// trailing ? marks an optional field.

export const PROTOCOL_VERSION = 4;
export const MIN_VERSION = PROTOCOL_VERSION - 1;

// Capabilities a host announces; the browser offers a feature only when its cap is there.
export const CAPS = Object.freeze({
  INPUT: 'input',
  FILES: 'files',
  CLIPBOARD: 'clipboard',
  CURSOR: 'cursor',
  DISPLAYS: 'displays',
  RELMOUSE: 'relmouse',
  LAYOUT: 'layout',
  CHARS: 'chars',
  CAPTURE: 'capture',
  DESK: 'desk',
  TERMINAL: 'terminal',
  TERMINAL_KEEP: 'terminal-keep',
  AI: 'ai',
  RELAY_RTT: 'relay-rtt',
});

const FORMAT = 's:text|html|image|files';
// The one list of reasons a session ends or is refused for, on bye and on the control channel's end.
export const REASONS = Object.freeze(['left', 'taken', 'ended', 'busy', 'failed', 'gone', 'offline', 'auth', 'unpaired', 'forbidden', 'permissions', 'no_desktop', 'no_terminal', 'pair', 'denied', 'full', 'ai_off', 'idle']);
const REASON = `s:${REASONS.join('|')}`;
const HOST = 's:online|away';
const CBERR = 's:cancelled|gone|denied|big|unsupported|io';

export const TYPES = Object.freeze({
  View: { w: 'i', h: 'i', maxW: 'i', maxH: 'i', fps: 'i' },
  ICEServer: { urls: '[s]', username: 's?', credential: 's?' },
  Display: { id: 's', x: 'i', y: 'i', w: 'i', h: 'i', primary: 'b' },
  Device: { id: 's', name: 's', os: 's', online: 'b', last_seen: 'i?', controller: 'Controller?', kept_terminal: 'KeptTerminal?', displays: '[Display]?', version: 'i?', caps: '[s]?', ai: 'AiView?', here: 'b?', tools: '[s]?' },
  AiView: { state: 's:off|on|stopped', admin: 'b', ts: 'i', name: 's?', since: 'i?', refused: 'b?' },
  Controller: { device: 'b', name: 's', connecting: 'b' },
  AuditQuery: { since: 'i?', until: 'i?', tool: 's?', client: 's?', failed: 'b?', offset: 'i?', limit: 'i' },
  AuditCall: { at: 'i', client: 's', device: 's', session: 's', tool: 's', args: 's', result: 's', detail: 's', ms: 'i?' },
  AuditPage: { calls: '[AuditCall]', total: 'i', clients: '[s]', tools: '[s]' },
  KeptTerminal: { since: 'i', name: 's', cred: 's?' },
  Holder: { session: 's', cred: 's?', name: 's', user: 's', established: 'b', since: 'i?' },
  Approval: { kind: 's:browser|device', id: 's', label: 's', ip: 's', code: 's?', created: 'i', device: 'a?' },
  IceCandidate: { candidate: 's', sdpMid: 's?', sdpMLineIndex: 'i?', usernameFragment: 's?' },
  OfferAuth: { k: 's:dk|rs', nonce: 's', cred: 's?', sig: 's?', id: 's?', mac: 's?' },
  AnswerAuth: { sig: 's', hk: 's' },
  VouchEntry: { cred: 's', alg: 'i', key: 's', name: 's' },
  Vouch: { list: '[VouchEntry]', sig: 's' },
  PairAuth: { cred: 's', alg: 'i', key: 's', name: 's', mac: 's?', vouch: 'Vouch?' },
  PairedAuth: { hk: 's', mac: 's?' },
  LogEntry: { op: 's:add|remove|policy|rename', cred: 's', alg: 'i?', key: 's?', name: 's?', ts: 'i', by: 's?', ad: 's?', cd: 's?', sig: 's?', keys: 'b?', role: 's:browser|ai?' },
  AiSwitch: { host: 's', ai: 'b', admin: 'b', ts: 'i', by: 's', ad: 's?', cd: 's?', sig: 's' },
  Mods: { shift: 'b', ctrl: 'b', alt: 'b', meta: 'b' },
  Locks: { caps: 'b', num: 'b', scroll: 'b' },
  Item: { n: 's', s: 'i' },
  Format: { kind: FORMAT, size: 'i?', w: 'i?', h: 'i?', items: '[Item]?' },
});

const B2S = 'browser->server', S2B = 'server->browser', H2B = 'host->browser', B2H = 'browser->host';
const msg = (dir, fields = {}) => Object.freeze({ dir: dir.split(','), fields: Object.freeze(fields) });

// Every message by channel and type: directions and field specs.
export const CHANNELS = Object.freeze({
  'signaling.browser': {
    discriminator: 'type',
    messages: Object.freeze({
      hello: msg(`${S2B},${B2S}`, { version: 'i', caps: '[s]', build: 's?' }),
      devices: msg(S2B, { devices: '[Device]' }),
      approvals: msg(S2B, { approvals: '[Approval]' }),
      ice: msg(S2B, { ice: '[ICEServer]' }),
      watch: msg(B2S, { on: 'b' }),
      locate: msg(B2S, { where: '[s]' }),
      auditreq: msg(B2S, { device: 's', query: 'AuditQuery' }),
      audit: msg(S2B, { device: 's', page: 'AuditPage' }),
      thumb: msg(S2B, { device: 's', screen: 's?', data: 's' }),
      challenge: msg(B2S, { session: 's', device: 's' }),
      nonce: msg(S2B, { session: 's', nonce: 's' }),
      offer: msg(B2S, { session: 's', device: 's', epoch: 'i', sdp: 's', view: 'View', force: 'b?', auth: 'OfferAuth?' }),
      answer: msg(S2B, { session: 's', epoch: 'i', sdp: 's', auth: 'AnswerAuth' }),
      restart: msg(B2S, { session: 's', epoch: 'i', sdp: 's' }),
      candidate: msg(`${B2S},${S2B}`, { session: 's', candidate: 'IceCandidate' }),
      attach: msg(B2S, { session: 's', device: 's' }),
      attached: msg(S2B, { session: 's', host: HOST }),
      presence: msg(S2B, { session: 's', host: HOST }),
      bye: msg(`${B2S},${S2B}`, { session: 's', reason: REASON, by: 's?', same: 'b?', controller: 'Controller?' }),
      pair: msg(B2S, { session: 's', device: 's', auth: 'PairAuth' }),
      pairask: msg(B2S, { session: 's', device: 's', auth: 'PairAuth' }),
      paired: msg(S2B, { session: 's', auth: 'PairedAuth' }),
    }),
  },
  control: {
    discriminator: 't',
    messages: Object.freeze({
      hello: msg(`${H2B},${B2H}`, { version: 'i', caps: '[s]' }),
      resume: msg(H2B, { id: 's', k: 's' }),
      end: msg(`${B2H},${H2B}`, { reason: REASON, by: 's?', same: 'b?' }),
      k: msg(B2H, { c: 's', d: 'b' }),
      kc: msg(B2H, { ch: 's', mods: 'Mods' }),
      x: msg(B2H, { v: 's' }),
      sync: msg(B2H, { locks: 'Locks', mods: 'Mods' }),
      b: msg(B2H, { b: 'i', d: 'b', x: 'n', y: 'n' }),
      w: msg(B2H, { dx: 'n', dy: 'n' }),
      m: msg(B2H, { x: 'n', y: 'n', s: 'i' }),
      mr: msg(B2H, { dx: 'n', dy: 'n', s: 'i' }),
      lc: msg(B2H, { on: 'b' }),
      view: msg(B2H, { w: 'i', h: 'i', maxW: 'i', maxH: 'i', fps: 'i' }),
      display: msg(B2H, { v: 's' }),
      'capture-keyframe': msg(B2H),
      displays: msg(H2B, { list: '[Display]', cur: 's' }),
      capture: msg(H2B, { v: 's:no-display|ready' }),
      desk: msg(H2B, { v: 's' }),
      cs: msg(H2B, { w: 'i', h: 'i', hx: 'i', hy: 'i', d: 's' }),
      layout: msg(H2B, { name: 's', labels: '{s}', shift: '{s}' }),
      clipshare: msg(`${B2H},${H2B}`, { on: 'b' }),
      cb: msg(`${B2H},${H2B}`, { gen: 'i?', owner: 's:host|browser?', formats: '[Format]', text: 's?', id: 's?' }),
      cbreq: msg(`${H2B},${B2H}`, { gen: 'i', format: FORMAT, id: 's', i: 'i?' }),
      cbdata: msg(`${B2H},${H2B}`, { gen: 'i', format: FORMAT, id: 's', text: 's?', html: 's?', size: 'i?' }),
      cberr: msg(`${B2H},${H2B}`, { gen: 'i', id: 's', why: CBERR }),
    }),
  },
  move: {
    discriminator: 't',
    messages: Object.freeze({
      m: msg(B2H, { x: 'n', y: 'n', s: 'i' }),
      mr: msg(B2H, { dx: 'n', dy: 'n', s: 'i' }),
      cp: msg(H2B, { x: 'n', y: 'n', v: 'b', s: 'i' }),
    }),
  },
  file: {
    discriminator: 't',
    messages: Object.freeze({
      put: msg(B2H, { id: 's', kind: 's:files|image|text|html', items: '[Item]', gen: 'i?' }),
      ready: msg(H2B, { id: 's' }),
      done: msg(H2B, { id: 's', names: '[s]' }),
      meta: msg(H2B, { id: 's', name: 's', size: 'i', mime: 's?', gen: 'i' }),
      fin: msg(H2B, { id: 's' }),
      cancel: msg(`${B2H},${H2B}`, { id: 's' }),
      err: msg(H2B, { id: 's', why: 's:big|locked|io|gone|unsupported|timeout|size|image' }),
    }),
  },
  terminal: {
    discriminator: 't',
    messages: Object.freeze({
      open: msg(B2H, { rows: 'i', cols: 'i' }),
      opened: msg(H2B, { replay: 'i?', dropped: 'b?' }),
      resize: msg(B2H, { rows: 'i', cols: 'i' }),
      ack: msg(`${B2H},${H2B}`, { bytes: 'i' }),
      close: msg(B2H),
      terminate: msg(B2H),
      closed: msg(H2B),
      exit: msg(H2B, { code: 'i' }),
      error: msg(H2B, { why: 's:open|close|io|protocol' }),
    }),
  },
});

// Type constants, so code names messages instead of spelling strings.
export const Signal = Object.freeze({
  HELLO: 'hello', DEVICES: 'devices', APPROVALS: 'approvals', ICE: 'ice', WATCH: 'watch', THUMB: 'thumb',
  CHALLENGE: 'challenge', NONCE: 'nonce', OFFER: 'offer', ANSWER: 'answer', RESTART: 'restart',
  CANDIDATE: 'candidate', ATTACH: 'attach', ATTACHED: 'attached', PRESENCE: 'presence', BYE: 'bye', PAIR: 'pair',
  PAIR_ASK: 'pairask', PAIRED: 'paired', LOCATE: 'locate', AUDIT_REQ: 'auditreq', AUDIT: 'audit',
});
export const Control = Object.freeze({
  HELLO: 'hello', RESUME: 'resume', END: 'end', KEY: 'k', CHAR: 'kc', TEXT: 'x', SYNC: 'sync',
  BUTTON: 'b', WHEEL: 'w', MOVE: 'm', MOVE_BY: 'mr', LOCAL_CURSOR: 'lc', VIEW: 'view', DISPLAY: 'display',
  CAPTURE_KEYFRAME: 'capture-keyframe', DISPLAYS: 'displays', CAPTURE: 'capture', DESK: 'desk',
  CURSOR_SHAPE: 'cs', LAYOUT: 'layout', CLIP_SHARE: 'clipshare', CLIPBOARD: 'cb',
  CLIPBOARD_REQUEST: 'cbreq', CLIPBOARD_DATA: 'cbdata', CLIPBOARD_ERROR: 'cberr',
});
export const Move = Object.freeze({ TO: 'm', BY: 'mr', CURSOR_POS: 'cp' });
// Not named File: that is the DOM's.
export const FileMsg = Object.freeze({ PUT: 'put', READY: 'ready', DONE: 'done', META: 'meta', FIN: 'fin', CANCEL: 'cancel', ERR: 'err' });
export const TerminalMsg = Object.freeze({
  OPEN: 'open', OPENED: 'opened', RESIZE: 'resize', ACK: 'ack', CLOSE: 'close', TERMINATE: 'terminate', CLOSED: 'closed', EXIT: 'exit', ERROR: 'error',
});
export const TERMINAL_FRAME = 16 * 1024;
export const TERMINAL_OUTPUT_WINDOW = 128 * 1024;
export const TERMINAL_INPUT_WINDOW = 64 * 1024;

// Parses a field spec into { kind, of, values, optional }.
function parseSpec(spec) {
  let s = spec;
  const optional = s.endsWith('?');
  if (optional) s = s.slice(0, -1);
  if (s.startsWith('[')) return { kind: 'array', of: s.slice(1, -1), optional };
  if (s.startsWith('{')) return { kind: 'map', of: s.slice(1, -1), optional };
  if (s.startsWith('s:')) return { kind: 'string', values: s.slice(2).split('|'), optional };
  const base = { s: 'string', i: 'integer', n: 'number', b: 'boolean', o: 'object', a: 'any' }[s];
  return base ? { kind: base, optional } : { kind: 'type', of: s, optional };
}

const isObject = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);

function checkValue(spec, v, path) {
  const p = typeof spec === 'string' ? parseSpec(spec) : spec;
  switch (p.kind) {
    case 'any': return null;
    case 'string':
      if (typeof v !== 'string') return `${path}: not a string`;
      return p.values && !p.values.includes(v) ? `${path}: ${JSON.stringify(v)} is not one of ${p.values.join(', ')}` : null;
    case 'integer': return Number.isInteger(v) ? null : `${path}: not an integer`;
    case 'number': return Number.isFinite(v) ? null : `${path}: not a number`;
    case 'boolean': return typeof v === 'boolean' ? null : `${path}: not a boolean`;
    case 'object': return isObject(v) ? null : `${path}: not an object`;
    case 'array':
      if (!Array.isArray(v)) return `${path}: not an array`;
      for (let i = 0; i < v.length; i++) {
        const e = checkValue(p.of, v[i], `${path}[${i}]`);
        if (e) return e;
      }
      return null;
    case 'map':
      if (!isObject(v)) return `${path}: not an object`;
      for (const [k, x] of Object.entries(v)) {
        const e = checkValue(p.of, x, `${path}.${k}`);
        if (e) return e;
      }
      return null;
    default:
      if (!TYPES[p.of]) return `${path}: unknown type ${p.of}`;
      return checkFields(TYPES[p.of], v, path, false);
  }
}

function checkFields(fields, v, path, strict) {
  if (!isObject(v)) return `${path}: not an object`;
  for (const [name, spec] of Object.entries(fields)) {
    const p = parseSpec(spec);
    if (v[name] === undefined) {
      if (!p.optional) return `${path}.${name}: missing`;
      continue;
    }
    const e = checkValue(p, v[name], `${path}.${name}`);
    if (e) return e;
  }
  if (strict) {
    for (const k of Object.keys(v)) if (!Object.hasOwn(fields, k)) return `${path}.${k}: not in the definition`;
  }
  return null;
}

// Checks a message for a channel: null when valid, else what is wrong. Unknown fields are
// allowed (a newer peer may add optional ones) unless `strict`.
export function validate(channel, m, { strict = false } = {}) {
  const ch = CHANNELS[channel];
  if (!ch) return `unknown channel ${channel}`;
  if (!isObject(m)) return 'not an object';
  const type = m[ch.discriminator];
  if (typeof type !== 'string' || !Object.hasOwn(ch.messages, type)) return `${channel}: unknown message ${JSON.stringify(type)}`;
  const rest = { ...m };
  delete rest[ch.discriminator];
  return checkFields(ch.messages[type].fields, rest, `${channel} ${type}`, strict);
}
