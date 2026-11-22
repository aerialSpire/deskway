// Contract test: the message builders in web/static/js/auth-messages.js against the golden strings in
// proto/vectors.json. Run: node --test tests/contract/vectors.test.mjs (no dependencies).
//
// Every vector the protocol marks "current" must be produced exactly. Vectors marked
// "new" are formats the web client does not build yet; they are skipped, and once the client builds
// one it gets a line in BUILDERS below (and its old version is deleted with its vector).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';
import * as auth from '../../web/static/js/auth-messages.js';

const all = JSON.parse(fs.readFileSync(fileURLToPath(new URL('../../proto/vectors.json', import.meta.url)), 'utf8'));
const sha = (s) => crypto.createHash('sha256').update(s).digest('hex');
const mac = (key, s) => crypto.createHmac('sha256', key).update(s).digest('base64');

// How auth.js builds each format from a vector's inputs (descriptions are hashed first, as the
// client does before signing).
const BUILDERS = {
  'deskway-connect-v3': (i) => auth.connectChallenge(i.host, i.session, i.nonce, i.force, sha(i.sdp)),
  'deskway-resume-v3': (i) => auth.resumeMac(i.host, i.session, i.nonce, i.force, sha(i.sdp)),
  'deskway-answer-v1': (i) => auth.answerMessage(i.host, i.session, sha(i.offer), sha(i.answer)),
  'deskway-endorse-v1': (i) => auth.endorseChallenge({ cred: i.cred, alg: i.alg, key: i.key, name: i.name }, i.ts),
  'deskway-remove-v1': (i) => auth.removeChallenge(i.cred, i.ts),
  'deskway-rename-v1': (i) => auth.renameChallenge(i.cred, i.name, i.ts),
  'deskway-policy-v1': (i) => auth.policyChallenge(i.keys, i.ts),
  'deskway-vouch-v1': (i) => auth.vouchChallenge(i.host, i.list),
  'deskway-code-v1': (i) => auth.keyCodeMessage(i.key),
  'deskway-code-v2': (i) => auth.keyCodeMessage(i.key, i.role),
  'deskway-endorse-v2': (i) => auth.endorseChallenge({ cred: i.cred, alg: i.alg, key: i.key, name: i.name, role: i.role }, i.ts),
  'deskway-ai-v1': (i) => auth.aiChallenge(i.host, i.ai, i.admin, i.ts),
  'deskway-pair-v1': (i) => auth.pairMessage(i.host, i.cred, i.alg, i.key, i.name),
  'deskway-paired-v1': (i) => auth.pairedMessage(i.host, i.cred, i.hostKey),
};

// Current formats only deskway and the server use: the web client neither builds nor checks them.
const NOT_WEB = new Set(['deskway-ai-login-v1']);

for (const v of all.vectors) {
  const build = BUILDERS[v.format];
  if (!build) {
    // A new format is skipped until the client builds it; a current one must have a builder.
    const skip = (v.status === 'new' && 'not built by the web client yet') || (NOT_WEB.has(v.format) && 'not a web format');
    test(v.format, { skip }, () => assert.fail('a current format with no builder in auth.js'));
    continue;
  }
  test(v.format, async () => {
    const s = await build(structuredClone(v.inputs));
    assert.equal(s, v.string);
    assert.equal(sha(s), v.sha256);
    if (v.mac_key) assert.equal(mac(Buffer.from(v.mac_key, 'base64'), s), v.mac);
    if (v.mac_key_text) {
      assert.equal(auth.normalizeCode(v.pairing_code), v.mac_key_text);
      assert.equal(mac(v.mac_key_text, s), v.mac);
    }
    if (v.code) assert.equal(await auth.keyCode(v.inputs.key, v.inputs.role), v.code);
  });
}
