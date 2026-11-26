// Contract test: web/static/js/proto.js against the shared message and version definitions.
// Run: node --test tests/contract/proto.test.mjs (no dependencies).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';
import * as proto from '../../web/static/js/proto.js';

const def = JSON.parse(fs.readFileSync(fileURLToPath(new URL('../../proto/messages.json', import.meta.url)), 'utf8'));
// The channels the browser speaks; signaling.host and ipc are the host's and the server's.
const BROWSER_CHANNELS = ['signaling.browser', 'control', 'move', 'file', 'terminal'];
const SHORT = { string: 's', integer: 'i', number: 'n', boolean: 'b', object: 'o', any: 'a' };

// The definition's field, written as a proto.js spec.
function specOf(f) {
  let s;
  if (f.enum) s = `s:${f.enum.join('|')}`;
  else if (f.type === 'array') s = `[${SHORT[f.items] || f.items}]`;
  else if (f.type === 'map') s = `{${SHORT[f.values] || f.values}}`;
  else s = SHORT[f.type] || f.type;
  return f.optional ? `${s}?` : s;
}

const specsOf = (fields) => Object.fromEntries(Object.entries(fields).map(([k, f]) => [k, specOf(f)]));


// A value for a field from the definition: every field (full) or required ones only.
function sample(f, full) {
  if (f.enum) return f.enum[0];
  switch (f.type) {
    case 'string': return 'x';
    case 'integer': return 1;
    case 'number': return 1.5;
    case 'boolean': return true;
    case 'object': case 'any': return {};
    case 'array': return [sample({ type: f.items }, full)];
    case 'map': return { KeyA: sample({ type: f.values }, full) };
    default: return object(def.types[f.type].fields, full);
  }
}

function object(fields, full) {
  return Object.fromEntries(Object.entries(fields).filter(([, f]) => full || !f.optional).map(([k, f]) => [k, sample(f, full)]));
}

test('version and capabilities', () => {
  const version = JSON.parse(fs.readFileSync(new URL('../../proto/version.json', import.meta.url), 'utf8'));
  assert.equal(proto.PROTOCOL_VERSION, version.protocol_version);
  assert.equal(proto.MIN_VERSION, version.protocol_version - 1);
  assert.deepEqual(Object.values(proto.CAPS), def.capabilities.filter((c) => c.who === 'host').map((c) => c.name));
});

test('shared types', () => {
  for (const name of Object.keys(proto.TYPES)) assert.ok(def.types[name], `${name} is not in messages.json`);
  for (const [name, t] of Object.entries(def.types)) assert.deepEqual(proto.TYPES[name], specsOf(t.fields), `type ${name}`);
});

test('channels, messages, directions and fields', () => {
  assert.deepEqual(Object.keys(proto.CHANNELS).sort(), [...BROWSER_CHANNELS].sort());
  for (const name of BROWSER_CHANNELS) {
    const ch = def.channels[name];
    const mine = proto.CHANNELS[name];
    assert.equal(mine.discriminator, ch.discriminator, `${name} discriminator`);
    assert.deepEqual(Object.keys(mine.messages).sort(), ch.messages.map((m) => m.type).sort(), `${name} message types`);
    for (const m of ch.messages) {
      assert.deepEqual([...mine.messages[m.type].dir].sort(), [...m.directions].sort(), `${name} ${m.type} directions`);
      assert.deepEqual({ ...mine.messages[m.type].fields }, specsOf(m.fields), `${name} ${m.type} fields`);
    }
  }
});

test('type constants', () => {
  const constants = { 'signaling.browser': proto.Signal, control: proto.Control, move: proto.Move, file: proto.FileMsg, terminal: proto.TerminalMsg };
  for (const [name, c] of Object.entries(constants)) {
    assert.deepEqual(Object.values(c).sort(), def.channels[name].messages.map((m) => m.type).sort(), `${name} constants`);
  }
  assert.equal(proto.TERMINAL_FRAME, def.channels.terminal.max_bytes);
  assert.equal(proto.TERMINAL_OUTPUT_WINDOW, def.channels.terminal.output_window_bytes);
  assert.equal(proto.TERMINAL_INPUT_WINDOW, def.channels.terminal.input_window_bytes);
});

test('the validator accepts what the definition allows and refuses the rest', () => {
  for (const name of BROWSER_CHANNELS) {
    const ch = def.channels[name];
    const tag = ch.discriminator;
    for (const m of ch.messages) {
      const full = { [tag]: m.type, ...object(m.fields, true) };
      const min = { [tag]: m.type, ...object(m.fields, false) };
      assert.equal(proto.validate(name, full, { strict: true }), null, `${name} ${m.type} full`);
      assert.equal(proto.validate(name, min, { strict: true }), null, `${name} ${m.type} minimal`);
      assert.notEqual(proto.validate(name, { ...full, zz: 1 }, { strict: true }), null, `${name} ${m.type} unknown field, strict`);
      assert.equal(proto.validate(name, { ...full, zz: 1 }), null, `${name} ${m.type} unknown field, lenient`);
      for (const [k, f] of Object.entries(m.fields)) {
        if (!f.optional) {
          const less = { ...full };
          delete less[k];
          assert.notEqual(proto.validate(name, less), null, `${name} ${m.type} without ${k}`);
        }
        if (f.enum) {
          for (const v of f.enum) assert.equal(proto.validate(name, { ...full, [k]: v }), null, `${name} ${m.type} ${k}=${v}`);
          assert.notEqual(proto.validate(name, { ...full, [k]: 'no-such-value' }), null, `${name} ${m.type} ${k} outside its values`);
        }
      }
    }
    assert.notEqual(proto.validate(name, { [tag]: 'no-such-message' }), null, `${name} unknown type`);
  }
  assert.notEqual(proto.validate('control', { t: 'b', b: 1.5, d: true, x: 0, y: 0 }), null, 'integer field');
  assert.notEqual(proto.validate('control', { t: 'k', c: 'KeyA', d: 'yes' }), null, 'boolean field');
});
