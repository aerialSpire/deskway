// End-to-end authentication (docs/security.md). The server only carries messages. A computer
// accepts a session only when a device key paired with it signs the offer, and it signs its
// answer so this page knows it reached that computer and not an impostor.
//
// The strings that are signed, MACed or hashed are built in auth-messages.js.
import { store, standalone } from './env.js';
import { t } from './i18n.js';
import { api } from './net.js';
import { b64, b64url, unb64, hexOf, sha256, hmac, policyChallenge, renameChallenge, aiChallenge, vouchChallenge, pairMessage, pairedMessage, connectChallenge, resumeMac, answerMessage } from './auth-messages.js';

const enc = new TextEncoder();

// ---- this device's key ----

// The device key's public half and id, kept where it can be read at once. The private half is
// in IndexedDB, as a key the page itself cannot read out: it can only sign with it.
export function storedPasskey() {
  try {
    const r = JSON.parse(store.get('deskway.passkey') || 'null');
    return r && r.cred && r.key && r.alg ? r : null;
  } catch { return null; }
}

function keyDb() {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open('deskway', 1);
    req.onupgradeneeded = () => req.result.createObjectStore('keys');
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

async function keyStore(mode, f) {
  const db = await keyDb();
  return new Promise((resolve, reject) => {
    const tx = db.transaction('keys', mode);
    const req = f(tx.objectStore('keys'));
    tx.oncomplete = () => resolve(req.result);
    tx.onerror = () => reject(tx.error);
  });
}

const privateKey = () => keyStore('readonly', (st) => st.get('device')).catch(() => null);

const hasDeviceKey = async () => !!storedPasskey() && !!(await privateKey());

export function deviceLabel() {
  const ua = navigator.userAgent;
  // iPadOS reports itself as a Mac; a touch screen gives it away.
  const ipad = /iPad/.test(ua) || (/Macintosh/.test(ua) && navigator.maxTouchPoints > 1);
  // Kept as the device's name, in the language it was added in.
  const kind = ipad ? 'iPad' : /iPhone/.test(ua) ? 'iPhone' : /Android/.test(ua) ? (/Mobile/.test(ua) ? t('device.androidPhone') : t('device.androidTablet')) : /Mac OS X/.test(ua) ? 'Mac' : /Windows/.test(ua) ? t('device.windows') : /Linux/.test(ua) ? t('device.linux') : t('device.browser');
  const browser = /Edg\//.test(ua) ? 'Edge' : /Firefox\//.test(ua) ? 'Firefox' : /Chrome\//.test(ua) ? 'Chrome' : /Safari\//.test(ua) ? 'Safari' : '';
  // Installed, it is told apart from the same browser in a tab (the Home Screen app is a
  // device of its own).
  if (standalone()) return `${kind} · ${t('device.app')}`.slice(0, 60);
  return (browser && !ipad && !/iPhone/.test(ua) ? `${kind} · ${browser}` : kind).slice(0, 60);
}

// Not through api(): this also runs while the device waits for approval, not signed in yet,
// where a 401 is no sign-out.
export async function reportPasskey(rec) {
  const res = await fetch('/api/login/device', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ cred: rec.cred, alg: rec.alg, key: rec.key, name: rec.name || deviceLabel() }) });
  // The browser cookie can outlive its key. The server keeps the key it already knows.
  if (!res.ok && res.status !== 404) throw new Error('device key registration failed');
  return res.ok;
}

// This device's key is made once, silently. A new key takes the chosen name. Tabs share
// the same key, including when two of them first sign in at the same time.
let provisioning;
export function ensurePasskey(name = deviceLabel()) {
  const prepare = () => loadOrCreatePasskey(name);
  provisioning ??= (navigator.locks ? navigator.locks.request('deskway.passkey', prepare) : prepare())
    .finally(() => { provisioning = null; });
  return provisioning;
}

async function loadOrCreatePasskey(name) {
  const have = storedPasskey();
  if (have && await privateKey()) return have;
  if (!window.crypto || !crypto.subtle || !window.indexedDB) throw new Error('nokey');
  const pair = await crypto.subtle.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, false, ['sign', 'verify']);
  const spki = await crypto.subtle.exportKey('spki', pair.publicKey);
  await keyStore('readwrite', (st) => st.put(pair.privateKey, 'device'));
  const rec = { cred: b64url(crypto.getRandomValues(new Uint8Array(16))), alg: -7, key: b64(spki), name };
  store.set('deskway.passkey', JSON.stringify(rec));
  // Ask the browser to keep the key through storage pressure and tracking prevention; the
  // devices page says so when it refused.
  await keyPersisted(true);
  return rec;
}

// Whether this site's storage (the device key with it) is kept until the person clears it.
// `ask`: request it (`navigator.storage.persist()`), else only report. Null when unknown.
export async function keyPersisted(ask = false) {
  const s = navigator.storage;
  if (!s || !s.persisted) return null;
  try {
    return (ask && s.persist) ? await s.persist() : await s.persisted();
  } catch { return null; }
}

// Signs with this device's key, without asking anything. `make(ts)` builds the message for the
// timestamp that goes into the proof (now, or `least` if that is later); what is signed is its
// SHA-256.
export async function signWith(make, least = 0) {
  const rec = storedPasskey();
  const priv = rec && await privateKey();
  if (!priv) throw new Error('nokey');
  const ts = Math.max(Math.floor(Date.now() / 1000), least);
  const sig = await crypto.subtle.sign({ name: 'ECDSA', hash: 'SHA-256' }, priv, await sha256(make(ts)));
  return { ts, by: rec.cred, sig: b64url(sig) };
}

// ---- computers' keys ----

const hostKeyName = (id) => `deskway.hostkey.${id}`;
export const pinnedHostKey = (id) => store.get(hostKeyName(id));
export const pinHostKey = (id, hk) => store.set(hostKeyName(id), hk);

// The auth for a session offer, over the computer's single-use `nonce`: a resume token if the
// computer gave one, else the device key. A browser with no key sends nothing, and the
// computer decides (device checks may be off).
export async function connectAuth({ hostId, session, nonce, sdp, force, resume }) {
  const sdpHash = hexOf(await sha256(sdp));
  if (resume) {
    const mac = await hmac(resume.k, resumeMac(hostId, session, nonce, force, sdpHash));
    return { k: 'rs', nonce, id: resume.id, mac: b64(mac) };
  }
  if (!(await hasDeviceKey())) return undefined;
  const p = await signWith(() => connectChallenge(hostId, session, nonce, force, sdpHash));
  return { k: 'dk', nonce, cred: p.by, sig: p.sig };
}

// Whether the computer's signature over both descriptions holds, with its key `hk`.
export async function answerSigned({ hostId, session, offerSdp, answerSdp, hk, sig }) {
  try {
    if (!sig || !hk || !offerSdp) return false;
    const key = await crypto.subtle.importKey('spki', unb64(hk), { name: 'ECDSA', namedCurve: 'P-256' }, false, ['verify']);
    const msg = answerMessage(hostId, session, hexOf(await sha256(offerSdp)), hexOf(await sha256(answerSdp || '')));
    return await crypto.subtle.verify({ name: 'ECDSA', hash: 'SHA-256' }, key, unb64(sig), enc.encode(msg));
  } catch { return false; }
}

// ---- the account ----

// Turns device checks off (the account password is enough) or back on. Signed by this device,
// so the server cannot do it on its own.
export async function setPolicy(keys) {
  const proof = await signWith((ts) => policyChallenge(keys, ts));
  const res = await api('POST', '/api/policy', { keys, proof });
  if (!res.ok) throw new Error('policy');
}

// Names one of the account's devices (`id`, its key `device`): this one or another. Signed, so
// the computers show the name too; a device without a key is named on the website only. False
// when it was not saved.
export async function renameDevice(id, device, name) {
  let proof;
  try {
    if (device) proof = await signWith((ts) => renameChallenge(device.cred, name, ts));
  } catch { /* this browser has no key: the website still keeps the name */ }
  const res = await api('PATCH', `/api/browsers/${encodeURIComponent(id)}`, { name, proof });
  if (!res.ok) return false;
  const mine = storedPasskey();
  if (mine && device && mine.cred === device.cred) store.set('deskway.passkey', JSON.stringify({ ...mine, name }));
  return true;
}

// Whether AI clients may control the computer `hostId`, and with administrator rights. Signed by
// this device; `after` is the time of the computer's latest switch: a new one is always later,
// whatever this device's clock says, or the computer would keep the old one.
export async function setAi(hostId, ai, admin, after) {
  const proof = await signWith((ts) => aiChallenge(hostId, ai, admin, ts), after + 1);
  const res = await api('POST', `/api/computers/${encodeURIComponent(hostId)}/ai`, { ai, admin, proof });
  if (!res.ok) throw new Error('ai');
}

// When this device is added at a computer, it vouches for the account's other devices, so the
// computer trusts all of them at once. Not for AI clients: they reach a computer only through
// a browser's endorsement and the computer's AI switch.
export async function vouchFor(hostId, self) {
  let rows = [];
  try {
    const res = await api('GET', '/api/devices');
    if (res.ok) rows = await res.json();
  } catch { /* vouch for nobody else */ }
  const list = rows.filter((r) => r.device && r.device.cred !== self.cred && r.device.role !== 'ai').map((r) => r.device);
  if (!list.length) return undefined;
  const p = await signWith(() => vouchChallenge(hostId, list));
  return { list, sig: p.sig };
}

// The pairing MAC for a QR code's `code`, and the check of the computer's reply.
export async function pairProof(hostId, rec, name, code) {
  const secret = enc.encode(code);
  const mac = await hmac(secret, pairMessage(hostId, rec.cred, rec.alg, rec.key, name));
  return {
    mac: b64(mac),
    replyValid: async (hk, replyMac) => !!hk && b64(await hmac(secret, pairedMessage(hostId, rec.cred, hk))) === replyMac,
  };
}
