import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { test } from 'node:test';
import { FILE_TRANSFER_FRAME as FRAME, FILE_TRANSFER_HEADER as HEADER, FILE_TRANSFER_WINDOW as WINDOW } from '../../web/static/js/proto.js';
import { chooseDownload, DOWNLOAD_LIMIT } from '../../web/static/js/session/download-file.js';
import { Sha256 } from '../../web/static/js/session/sha256.js';
globalThis.matchMedia = () => ({ matches: false });
globalThis.document = { getElementById: () => null };
const { FileTransfers } = await import('../../web/static/js/session/file-transfers.js');

const chunk = FRAME - HEADER;
const turn = () => new Promise((resolve) => setImmediate(resolve));
const digest = (data) => createHash('sha256').update(data).digest('hex');
const file = (name, length = 0) => new File([new Uint8Array(length)], name);

function fixture() {
  const sent = [];
  const refreshed = [];
  const transfers = new FileTransfers(null, () => !!channel, () => {}, (path) => refreshed.push(path));
  const connect = () => {
    const next = new EventTarget();
    next.readyState = 'open';
    next.send = (data) => sent.push(typeof data === 'string' ? JSON.parse(data) : data);
    next.close = () => next.dispatchEvent(new Event('close'));
    channel = next;
    transfers.attach(channel);
    transfers.next();
  };
  let channel;
  connect();
  const recover = () => { channel = null; transfers.recover(); };
  const message = (m) => transfers.receive(JSON.stringify({ id: transfers.current.id, ...m }));
  const ready = (size = transfers.current.size, offset = 0) => message({ t: 'ready', size, offset });
  const frames = () => sent.filter((data) => data instanceof ArrayBuffer);
  const card = (job) => transfers.list.card(job);
  return { transfers, sent, refreshed, message, ready, recover, connect, frames, card };
}

async function finishUpload(f) {
  f.ready();
  await turn();
  f.message({ t: 'done', offset: f.transfers.current.size });
}

function frame(id, offset, length = chunk, fill = 0) {
  const data = new ArrayBuffer(HEADER + length);
  const header = new DataView(data);
  header.setUint32(0, id);
  header.setBigUint64(4, BigInt(offset));
  new Uint8Array(data, HEADER).fill(fill);
  return data;
}

test('the page lists progress, waiting, interrupted, finished, failed and cancelled transfers with their actions', async () => {
  const f = fixture();
  const path = ['folder'];
  f.transfers.upload(path, ['a', 'b', 'c', 'd'].map((name) => file(name)));
  const [a, b, c, d] = f.transfers.jobs;
  assert.equal(f.card(a).pct, 0);
  assert.equal(f.card(b).detail, 'files.transfer.waiting');
  f.card(d).actions[0].run();
  assert.match(f.card(d).label, /files.transfer.failure/);
  f.recover();
  assert.equal(f.card(a).detail, 'files.transfer.interrupted');
  assert.equal(f.card(b).detail, 'files.transfer.waiting');
  f.connect();
  await finishUpload(f);
  assert.deepEqual(f.refreshed, [path]);
  assert.equal(f.card(a).tone, 'ok');
  assert.match(f.card(a).detail, /files.transfer.verified/);
  f.message({ t: 'error', why: 'integrity' });
  assert.equal(f.card(b).tone, 'bad');
  assert.equal(f.card(b).actions[0].text, 'files.transfer.tryAgain');
  f.card(b).actions[0].run();
  f.card(a).close.run();
  assert.deepEqual(f.transfers.jobs.map(({ request }) => request.name), ['c', 'd', 'b']);
  f.card(c).close.run();
  assert.equal(f.sent.at(-1).t, 'cancel');
  f.message({ t: 'error', why: 'cancelled' });
  assert.equal(f.transfers.current.request.name, 'b');
  f.transfers.destroy();
  assert.deepEqual(f.transfers.jobs, []);
});

test('batch collision answers apply only to later conflicts in the same batch', async () => {
  for (const choice of ['skip', 'replace']) {
    const f = fixture();
    const prompts = [];
    f.transfers.list.conflict = (...args) => prompts.push(args);
    const conflict = { t: 'conflict', size: 13, modified: 123 };
    f.transfers.upload([], ['a', 'b', 'c'].map((name) => file(name)));
    f.transfers.upload([], [file('next batch')]);
    f.message(conflict);
    assert.equal(prompts.length, 1);
    const [, metadata, many, answer] = prompts[0];
    assert.deepEqual(metadata, { id: f.transfers.current.id, ...conflict });
    assert.equal(many, true);
    assert.deepEqual(f.sent.map((m) => m.t), ['upload']);
    answer(choice, true);
    assert.equal(f.sent.at(-1).t, choice === 'replace' ? 'replace' : 'cancel');
    if (choice === 'replace') await finishUpload(f);
    else f.message({ t: 'error', why: 'cancelled' });

    await finishUpload(f); // No collision, even after choosing to skip the rest.
    f.message(conflict);
    assert.equal(prompts.length, 1);
    assert.equal(f.sent.at(-1).t, choice === 'replace' ? 'replace' : 'cancel');
    if (choice === 'replace') await finishUpload(f);
    else f.message({ t: 'error', why: 'cancelled' });
    f.message(conflict);
    assert.equal(prompts.length, 2);
    assert.equal(prompts.at(-1)[2], false);
  }
});

test('upload keeps its credit limit and hashes correctly when the receiver is ahead of or behind the last acknowledgment', async () => {
  for (const resume of [0, 2 * chunk]) {
    const f = fixture();
    const size = WINDOW + chunk + 1;
    f.transfers.upload([], [file('a', size)]);
    const { id } = f.transfers.current;
    f.ready();
    await turn();
    assert.equal(f.frames().length, 4);
    assert(f.frames().every((frame) => frame.byteLength === FRAME));
    f.message({ t: 'ack', offset: chunk });
    await turn();
    assert.equal(f.frames().length, 5);
    assert.equal(new DataView(f.frames().at(-1)).getBigUint64(4), BigInt(WINDOW));
    f.recover();
    f.connect();
    assert.deepEqual(f.sent.at(-1), { t: 'resume', id });
    f.ready(size, resume);
    await turn();
    assert.equal(new DataView(f.frames()[5]).getBigUint64(4), BigInt(resume));
    f.message({ t: 'ack', offset: Math.min(size, resume + WINDOW) });
    await turn();
    if (resume === 0) f.message({ t: 'ack', offset: size });
    assert.deepEqual(f.sent.at(-1), { t: 'finish', id, offset: size, sha256: digest(Buffer.alloc(size)) });
    f.recover(); // The reply may have been lost after the host committed.
    f.connect();
    assert.deepEqual(f.sent.at(-1), { t: 'resume', id });
    f.message({ t: 'done', offset: size });
    assert.equal(f.card(f.transfers.jobs[0]).tone, 'ok');
  }
});

async function target(f, size, writer) {
  f.transfers.download([], 'id', 'name', 'name');
  await turn();
  f.transfers.current.request.target = { open: () => writer };
  f.ready(size);
  await turn();
}

test('a stalled download writer returns no credit and excess data is refused', async () => {
  const f = fixture();
  let resume;
  const paused = new Promise((resolve) => { resume = resolve; });
  let writes = 0;
  let aborted = 0;
  await target(f, 6 * chunk, {
    write: () => { writes++; return paused; },
    abort: () => { aborted++; },
  });
  const { id } = f.transfers.current;
  for (let i = 0; i < 4; i++) f.transfers.receive(frame(id, i * chunk));
  await turn();
  assert.equal(writes, 1);
  assert.equal(f.sent.at(-1).t, 'start');
  f.transfers.receive(frame(id, 4 * chunk));
  assert.equal(f.sent.at(-1).why, 'protocol');
  resume();
  await turn();
  assert.equal(aborted, 1);
  assert.equal(writes, 1);
  assert(!f.sent.some((m) => m.t === 'ack'));
});

test('a download resumes at the written offset and compares the digest before saving', async () => {
  for (const match of [true, false]) {
    const f = fixture();
    let releaseWrite;
    const paused = new Promise((resolve) => { releaseWrite = resolve; });
    const calls = [];
    await target(f, 2 * chunk, {
      write: (data) => { calls.push(data[0]); return paused; },
      close: () => calls.push('close'),
      abort: () => calls.push('abort'),
    });
    const { id } = f.transfers.current;
    f.transfers.receive(frame(id, 0, chunk, 1));
    await turn();
    f.recover();
    f.connect();
    await turn();
    assert.deepEqual(f.sent.at(-1), { t: 'resume', id });
    f.ready(2 * chunk);
    await turn();
    assert.equal(f.sent.at(-1).t, 'resume');
    releaseWrite();
    await turn();
    assert(!f.sent.some((m) => m.t === 'ack'));
    assert.deepEqual(f.sent.at(-1), { t: 'start', id, offset: chunk });
    f.transfers.receive(frame(id, chunk, chunk, 2));
    await turn();
    const sha256 = digest(Buffer.concat([Buffer.alloc(chunk, 1), Buffer.alloc(chunk, match ? 2 : 3)]));
    f.message({ t: 'finish', offset: 2 * chunk, sha256 });
    await turn();
    assert.deepEqual(calls, [1, 2, match ? 'close' : 'abort']);
    if (match) {
      assert.equal(f.sent.at(-1).t, 'done');
      f.message({ t: 'done', offset: 2 * chunk });
    } else {
      assert.equal(f.sent.at(-1).why, 'integrity');
      f.message({ t: 'error', why: 'integrity' });
    }
    assert.equal(f.card(f.transfers.jobs[0]).tone, match ? 'ok' : 'bad');
  }
});

test('a new session view on the page gets a new transfer id', () => {
  const first = fixture();
  first.transfers.upload([], [file('one')]);
  first.transfers.destroy();
  const second = fixture();
  second.transfers.upload([], [file('two')]);
  assert(second.sent[0].id > first.sent[0].id);
});

test('incremental SHA-256 matches the platform implementation at block and padding boundaries', () => {
  for (const size of [0, 1, 55, 56, 63, 64, 65, 127, 128, chunk * 2 + 37]) {
    const data = Buffer.alloc(size);
    for (let i = 0; i < size; i++) data[i] = i % 251;
    const hash = new Sha256();
    for (let i = 0; i < size; i += 31) hash.update(data.subarray(i, i + 31));
    assert.equal(hash.finish(), digest(data), 'size ' + size);
  }
});

test('the browser selects a streamed writer or a 64 MiB Blob fallback', async () => {
  const calls = [];
  const stream = await chooseDownload('log.txt', {
    showSaveFilePicker: async (options) => {
      calls.push(options);
      return { createWritable: async () => ({
        write: (bytes) => calls.push(bytes), close: () => calls.push('close'), abort: () => calls.push('abort'),
      }) };
    },
  });
  assert.equal(stream.limit, undefined);
  const writer = await stream.open(DOWNLOAD_LIMIT + 1);
  assert.deepEqual(calls, [{ suggestedName: 'log.txt' }]);
  writer.write(new Uint8Array([97]));
  writer.abort();
  assert.deepEqual(calls, [{ suggestedName: 'log.txt' }, new Uint8Array([97]), 'abort']);

  const fallback = await chooseDownload('log.txt', {});
  assert.equal(fallback.limit, DOWNLOAD_LIMIT);
  assert.equal(fallback.open(DOWNLOAD_LIMIT + 1), null);
  let result;
  const memory = fallback.open(DOWNLOAD_LIMIT, (blob, name) => { result = { blob, name }; });
  memory.write(new Uint8Array([97]));
  memory.close();
  assert.equal(result.name, 'log.txt');
  assert.equal(await result.blob.text(), 'a');
});

test('the download offer tells the host the Blob limit and a refusal starts no data', async () => {
  const f = fixture();
  f.transfers.download([], 'id', 'name', 'name');
  await turn();
  assert.equal(f.sent[0].limit, DOWNLOAD_LIMIT);
  f.message({ t: 'error', why: 'limit' });
  assert.equal(f.sent.length, 1);
  assert.equal(f.card(f.transfers.jobs[0]).tone, 'bad');
  assert.equal(f.transfers.current, null);
});
