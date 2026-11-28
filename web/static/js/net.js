// The server: its HTTP API, the signaling socket, and the ICE servers it hands out.
import { CAPS, PROTOCOL_VERSION, Signal } from './proto.js';

let unauthorized = () => {};

// What happens when the server says this browser is no longer signed in.
export function onUnauthorized(fn) { unauthorized = fn; }

// The sign-in page is already showing, so callers say nothing about this error.
const signedOutError = () => Object.assign(new Error('unauthorized'), { signedOut: true });
export const signedOut = (err) => !!(err && err.signedOut);

export async function api(method, path, body) {
  const res = await fetch(path, {
    method,
    headers: body ? { 'Content-Type': 'application/json' } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  if (res.status === 401 && path !== '/api/login') {
    unauthorized();
    throw signedOutError();
  }
  return res;
}

// The STUN and TURN servers, from the server on every socket and renewed before they expire.
let ice = null;
const iceWaiting = [];
export function setIce(list) {
  ice = list;
  for (const resolve of iceWaiting.splice(0)) resolve(list);
}
// The ICE servers, as soon as the server has sent them.
export const iceServers = () => (ice ? Promise.resolve(ice) : new Promise((resolve) => iceWaiting.push(resolve)));

// The signaling socket. It reconnects by itself (backing off up to 15 s) until closed, and
// keeps messages sent while it is down. `onDown` is called only when an outage lasts: a blip
// that heals within two seconds is not worth a message.
class Signaling {
  constructor() {
    this.ws = null;
    this.wanted = false;
    this.delay = 1000;
    this.retryTimer = null;
    this.staleTimer = null;
    this.pending = [];
    this.onMessage = () => {};
    this.onUp = () => {};
    this.onDown = () => {};
    this.serverCaps = [];
  }

  connect() {
    this.wanted = true;
    clearTimeout(this.retryTimer);
    const scheme = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const ws = new WebSocket(`${scheme}//${location.host}/ws/client`);
    this.ws = ws;
    ws.onopen = () => {
      this.delay = 1000;
      clearTimeout(this.staleTimer);
      ws.send(JSON.stringify({ type: Signal.HELLO, version: PROTOCOL_VERSION, caps: [CAPS.TERMINALS] }));
      this.onUp();
      for (const m of this.pending.splice(0)) ws.send(JSON.stringify(m));
    };
    ws.onmessage = (e) => {
      if (this.ws !== ws) return;
      const message = JSON.parse(e.data);
      if (message.type === Signal.HELLO) this.serverCaps = message.caps ?? [];
      this.onMessage(message);
    };
    ws.onclose = () => {
      if (!this.wanted || this.ws !== ws) return;
      clearTimeout(this.staleTimer);
      this.staleTimer = setTimeout(() => this.onDown(), 2000);
      this.retryTimer = setTimeout(() => this.connect(), this.delay);
      this.delay = Math.min(this.delay * 2, 15000);
    };
  }

  // A page back at the front (a phone woken, a tab returned to) may hold a socket the network
  // dropped while it slept: the browser can take minutes to notice, and until then nothing the
  // server sends arrives (a device asking to be let in). This starts afresh, quietly (no outage
  // is shown); the new socket's hello brings everything again, and a session claims itself back.
  renew() {
    if (!this.wanted) return;
    const old = this.ws;
    clearTimeout(this.retryTimer);
    clearTimeout(this.staleTimer);
    this.delay = 1000;
    this.connect();
    if (old) old.close();
  }

  close() {
    this.wanted = false;
    clearTimeout(this.retryTimer);
    clearTimeout(this.staleTimer);
    if (this.ws) this.ws.close();
  }

  isOpen() { return !!this.ws && this.ws.readyState === WebSocket.OPEN; }

  hasCap(cap) { return this.serverCaps.includes(cap); }

  send(m) {
    if (this.isOpen()) this.ws.send(JSON.stringify(m));
    else this.pending.push(m);
  }

  // Only sent while open: the next open sends it again anyway.
  sendNow(m) { if (this.isOpen()) this.ws.send(JSON.stringify(m)); }
}

export const socket = new Signaling();
