import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

globalThis.document = {
  documentElement: {},
  getElementById: () => null,
  addEventListener() {},
  createElement: (tag) => ({
    tag, children: [], attrs: {},
    setAttribute(key, value) { this.attrs[key] = value; },
    append(child) { this.children.push(child); },
    get textContent() { return this.children.map((child) => child.textContent ?? child).join(''); },
  }),
};
globalThis.matchMedia = () => ({ matches: false });
globalThis.fetch = async (path) => ({
  ok: true, json: () => JSON.parse(readFileSync(new URL(`../../web/static/${path}`, import.meta.url), 'utf8')),
});
const { setLanguage } = await import('../../web/static/js/i18n.js');
const { statusLine } = await import('../../web/static/js/views/common.js');
const { vitalsBars } = await import('../../web/static/js/views/vitals.js');
await setLanguage('en');

const disk = (name, used, total) => ({ name, used, total });
const device = {
  online: true, version: 5, app: '0.1.3', os: 'windows',
  machine: { cpu: 'Intel Core i7', cores: 12, threads: 20, memory: 16e9 },
  vitals: {
    cpu: 79,
    memory: { used: 8e9, total: 16e9 },
    disks: [disk('D:', 0.65e12, 2e12), disk('C:', 486.4e9, 512e9)],
  },
};

test('status shows app version and measured latency, with four signal levels', () => {
  for (const [rtt, bars] of [[0, 4], [29, 4], [30, 3], [79, 3], [80, 2], [149, 2], [150, 1]]) {
    const { children: [dot, presence, latency, version] } = statusLine({ ...device, vitals: { rtt } }).children[0];
    assert.equal(dot.className, 'dot ok');
    assert.equal(presence.textContent, 'Online ·');
    assert.equal(latency.textContent, rtt ? `${rtt} ms` : '<1 ms');
    assert.equal(latency.className, bars === 1 ? 'lat warn' : 'lat');
    assert.equal(latency.children[0].children.length, 4);
    assert.equal(latency.children[0].children.filter((bar) => bar.className === 'on').length, bars);
    assert.equal(version.textContent, 'v0.1.3');
  }
});

test('offline status keeps the version and version 4 still shows the update hint', () => {
  const offline = { ...device, online: false, vitals: { ...device.vitals, rtt: 12 } };
  assert.deepEqual(statusLine(offline).children[0].children.map((node) => node.textContent), ['', 'Offline', 'v0.1.3']);
  const legacy = { online: true, version: 4 };
  const [row, update] = statusLine(legacy).children;
  assert.equal(row.textContent, 'Online');
  assert.equal(update.className, 'computer-update');
  assert.equal(update.textContent, 'Update available');
  assert.equal(statusLine(device).children[0].children.some((node) => node.className === 'lat'), false);
  for (const input of [offline, legacy, { ...device, vitals: undefined }]) {
    assert.equal(vitalsBars(input), null);
    assert.equal(vitalsBars(input, { detail: true }), null);
  }
});

test('list shows CPU, memory and the system disk with usage thresholds', () => {
  for (const [cpu, level] of [[79.9, 'gauge'], [80, 'gauge warn'], [94.9, 'gauge warn'], [95, 'gauge high']]) {
    const { children } = vitalsBars({ ...device, vitals: { ...device.vitals, cpu } });
    assert.deepEqual(children.map((gauge) => gauge.textContent), [`CPU${Math.round(cpu)}%`, 'RAM50%', 'Disk95%']);
    assert.equal(children[0].className, level);
    assert.equal(children[0].children[2].children[0].attrs.style, `width: ${Math.round(cpu)}%`);
  }
  const linux = { ...device, os: 'linux', vitals: { ...device.vitals, disks: [disk('/home', 5, 10), disk('/', 2, 10)] } };
  assert.equal(vitalsBars(linux).children[2].textContent, 'Disk20%');
  assert.equal(vitalsBars({ ...linux, vitals: { ...linux.vitals, disks: [] } }).children.length, 2);
});

test('detail includes the CPU model, topology and capacities for every disk', () => {
  const card = vitalsBars(device, { detail: true });
  assert.equal(card.tag, 'section');
  assert.equal(card.attrs['aria-label'], 'Vitals');
  assert.deepEqual(card.children[0].children.map((gauge) => gauge.children[3]?.textContent), [
    'Intel Core i7 · 12 cores 20 threads', '8.0 GB used of 16 GB',
    '0.7 TB used of 2.0 TB', '486 GB used of 512 GB',
  ]);
  assert.deepEqual(card.children[0].children.map((gauge) => gauge.children[0].textContent), ['CPU', 'Memory', 'Disk D:', 'Disk C:']);
  const noMachine = vitalsBars({ ...device, machine: undefined }, { detail: true });
  assert.equal(noMachine.children[0].children[0].children.length, 3);
});
