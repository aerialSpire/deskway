// The exact strings Deskway signs, MACs or hashes, and the encodings around them. Pure: no
// storage, network or page, so the contract test (tests/contract/vectors.test.mjs) checks it
// against proto/vectors.json under Node, and the computers build the same strings.
const enc = new TextEncoder();
const u8 = (b) => (b instanceof Uint8Array ? b : new Uint8Array(b));

export function b64(bytes) {
  let s = '';
  for (const x of u8(bytes)) s += String.fromCharCode(x);
  return btoa(s);
}
export const b64url = (bytes) => b64(bytes).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
export const unb64 = (s) => Uint8Array.from(atob(s.replace(/-/g, '+').replace(/_/g, '/')), (c) => c.charCodeAt(0));
export const hexOf = (b) => Array.from(u8(b), (x) => x.toString(16).padStart(2, '0')).join('');
export const sha256 = async (data) => new Uint8Array(await crypto.subtle.digest('SHA-256', typeof data === 'string' ? enc.encode(data) : data));
export async function hmac(key, msg) {
  const k = await crypto.subtle.importKey('raw', key, { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
  return new Uint8Array(await crypto.subtle.sign('HMAC', k, enc.encode(msg)));
}

// Letters and digits only, upper case, with O read as 0 and I or L as 1, as the host does.
export const normalizeCode = (c) => c.replace(/[^0-9a-z]/gi, '').toUpperCase().replace(/O/g, '0').replace(/[IL]/g, '1');


// ---- the formats ----

// Six digits from a device's key, shown on both devices to compare. An AI client's role is part
// of them (code-v2): a server that called it a browser would show other digits on the two sides.
export const keyCodeMessage = (key, role) => (role === 'ai' ? `deskway-code-v2\nai\n${key}` : `deskway-code-v1\n${key}`);
export async function keyCode(key, role) {
  const d = await sha256(keyCodeMessage(key, role));
  return String(new DataView(d.buffer).getUint32(0) % 1000000).padStart(6, '0');
}
// A signed-in device lets a new one in. An AI client's role is signed (endorse-v2), so it cannot
// pass for a browser; a browser keeps endorse-v1, which computers from before roles check too.
export const endorseChallenge = (dev, ts) => (dev.role === 'ai'
  ? `deskway-endorse-v2\n${dev.cred}\n${dev.alg}\n${dev.key}\n${dev.name}\nai\n${ts}`
  : `deskway-endorse-v1\n${dev.cred}\n${dev.alg}\n${dev.key}\n${dev.name}\n${ts}`);
// Whether AI clients may control one computer, and with administrator rights.
export const aiChallenge = (hostId, ai, admin, ts) => `deskway-ai-v1\n${hostId}\n${ai ? 1 : 0}\n${admin ? 1 : 0}\n${ts}`;
// A device is taken off the account.
export const removeChallenge = (cred, ts) => `deskway-remove-v1\n${cred}\n${ts}`;
// A device of the account gets a new name.
export const renameChallenge = (cred, name, ts) => `deskway-rename-v1\n${cred}\n${name}\n${ts}`;
// Device checks on or off.
export const policyChallenge = (keys, ts) => `deskway-policy-v1\n${keys ? 1 : 0}\n${ts}`;
// The account's other devices, vouched for when this one is added at a computer. Sorted by id.
export function vouchChallenge(hostId, list) {
  const sorted = [...list].sort((a, b) => (a.cred < b.cred ? -1 : a.cred > b.cred ? 1 : 0));
  return `deskway-vouch-v1\n${hostId}\n` + sorted.map((c) => `${c.cred}\n${c.alg}\n${c.key}\n`).join('');
}
// Pairing with the code from the computer's QR code (HMAC with the code), and its reply.
export const pairMessage = (hostId, cred, alg, key, name) => `deskway-pair-v1\n${hostId}\n${cred}\n${alg}\n${key}\n${name}`;
export const pairedMessage = (hostId, cred, hk) => `deskway-paired-v1\n${hostId}\n${cred}\n${hk}`;
// A session offer, signed with the device key over the computer's single-use `nonce`;
// `sdpHash` is the hex SHA-256 of the offer.
export const connectChallenge = (hostId, session, nonce, force, sdpHash) => `deskway-connect-v3\n${hostId}\n${session}\n${nonce}\n${force ? 1 : 0}\n${sdpHash}`;
// The same, MACed with the resume token the computer gave for an earlier session.
export const resumeMac = (hostId, session, nonce, force, sdpHash) => `deskway-resume-v3\n${hostId}\n${session}\n${nonce}\n${force ? 1 : 0}\n${sdpHash}`;
// The computer's answer, signed with its host key over both descriptions' hashes.
export const answerMessage = (hostId, session, offerHash, answerHash) => `deskway-answer-v1\n${hostId}\n${session}\n${offerHash}\n${answerHash}`;
