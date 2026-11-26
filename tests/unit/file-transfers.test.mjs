import assert from 'node:assert/strict';
import { test } from 'node:test';
import { FILE_TRANSFER_FRAME, FILE_TRANSFER_HEADER, FILE_TRANSFER_WINDOW } from '../../web/static/js/proto.js';
import { chooseDownload, DOWNLOAD_LIMIT } from '../../web/static/js/session/download-file.js';
globalThis.matchMedia = () => ({ matches: false });
globalThis.document = { getElementById: () => null };
const { FileTransfers } = await import('../../web/static/js/session/file-transfers.js');

const chunk = FILE_TRANSFER_FRAME - FILE_TRANSFER_HEADER;
const turn = () => new Promise((resolve) => setImmediate(resolve));

function fixture() {
  const sent = [];
  const refreshed = [];
  const labels = [];
  const channel = new EventTarget();
  channel.readyState = 'open';
  channel.send = (data) => sent.push(typeof data === 'string' ? JSON.parse(data) : data);
  channel.close = () => channel.dispatchEvent(new Event('close'));
  const transfers = new FileTransfers(null, () => true, () => {}, (path) => refreshed.push(path));
  const progress = {};
  transfers.progress = {
    draw: (card) => { progress.card = card; },
    remove: () => { progress.card = null; },
    setDetail() {}, setPercent() {},
  };
  transfers.failure = { draw: ({ label }) => labels.push(label), remove() {} };
  transfers.attach(channel);
  const message = (m) => transfers.receive(JSON.stringify({ id: transfers.current.id, ...m }));
  return { channel, transfers, sent, refreshed, labels, message, progress };
}

const file = (name, length = 0) => new File([new Uint8Array(length)], name);
async function finishUpload({ transfers, message }) {
  message({ t: 'ready', size: transfers.current.size });
  await turn();
  message({ t: 'done', offset: transfers.current.size });
}

test('the transfer queue starts files in order and refreshes successful uploads', async () => {
  const f = fixture();
  const path = ['folder'];
  f.transfers.upload(path, ['a', 'b', 'c'].map((name) => file(name)));
  assert.equal(f.progress.card.more, 2);
  assert.deepEqual(f.sent.map((m) => m.name), ['a']);
  for (let i = 0; i < 3; i++) await finishUpload(f);
  assert.deepEqual(f.sent.filter((m) => m.t === 'upload').map((m) => m.name), ['a', 'b', 'c']);
  assert.deepEqual(f.refreshed, [path, path, path]);
  assert.equal(f.progress.card, null);
});

test('cancel one, cancel waiting, close, and recover keep the queue ordered', async () => {
  for (const action of ['one', 'waiting', 'close', 'recover']) {
    const f = fixture();
    f.transfers.upload([], ['a', 'b', 'c'].map((name) => file(name)));
    if (action === 'one') {
      f.transfers.stop(f.transfers.current);
      assert.equal(f.sent.at(-1).t, 'cancel');
      assert.equal(f.progress.card.more, 2);
      f.message({ t: 'error', why: 'cancelled' });
    } else if (action === 'waiting') {
      f.transfers.cancelWaiting();
      assert.equal(f.progress.card.more, 0);
      await finishUpload(f);
    } else if (action === 'close') {
      f.channel.close();
    } else {
      f.transfers.recover();
      assert.equal(f.sent.at(-1).t, 'cancel');
      f.message({ t: 'error', why: 'cancelled' });
    }
    assert.equal(f.progress.card?.more, action === 'one' ? 1 : undefined, action);
    assert.deepEqual(f.sent.filter((m) => m.t === 'upload').map((m) => m.name), action === 'one' ? ['a', 'b'] : ['a']);
  }
});

test('a stalled upload receiver keeps the sender within its frame and credit limits', async () => {
  const f = fixture();
  const size = FILE_TRANSFER_WINDOW + chunk + 1;
  f.transfers.upload([], [file('a', size)]);
  f.message({ t: 'ready', size });
  await turn();
  const frames = () => f.sent.filter((m) => m instanceof ArrayBuffer);
  assert.equal(frames().length, 4);
  assert(frames().every((frame) => frame.byteLength === FILE_TRANSFER_FRAME));
  await turn();
  assert.equal(frames().length, 4);
  f.message({ t: 'ack', offset: chunk });
  await turn();
  assert.equal(frames().length, 5);
  const last = new DataView(frames().at(-1));
  assert.equal(last.getUint32(0), f.transfers.current.id);
  assert.equal(last.getBigUint64(4), BigInt(FILE_TRANSFER_WINDOW));
  f.message({ t: 'ack', offset: FILE_TRANSFER_WINDOW + chunk });
  await turn();
  assert.equal(frames().at(-1).byteLength, FILE_TRANSFER_HEADER + 1);
  f.message({ t: 'ack', offset: size });
  assert.deepEqual(f.sent.at(-1), { t: 'finish', id: 1, offset: size });
});

test('a stalled download writer returns no credit and excess data is refused', async () => {
  const f = fixture();
  f.transfers.download([], 'id', 'name', 'name');
  await turn();
  let resume;
  const paused = new Promise((resolve) => { resume = resolve; });
  let writes = 0;
  let aborted = 0;
  f.transfers.current.target = { open: () => ({
    write: () => { writes++; return paused; },
    abort: () => { aborted++; },
  }) };
  f.message({ t: 'ready', size: 6 * chunk });
  await turn();
  const frame = (i) => {
    const bytes = new ArrayBuffer(FILE_TRANSFER_FRAME);
    const header = new DataView(bytes);
    header.setUint32(0, 1);
    header.setBigUint64(4, BigInt(i * chunk));
    f.transfers.receive(bytes);
  };
  for (let i = 0; i < 4; i++) frame(i);
  await turn();
  assert.equal(writes, 1);
  assert.equal(f.sent.at(-1).t, 'start');
  frame(4);
  assert.equal(f.sent.at(-1).why, 'protocol');
  assert.equal(aborted, 1);
  resume();
  await turn();
  assert.equal(writes, 1);
  assert(!f.sent.some((m) => m.t === 'ack'));
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
  assert.equal(f.labels.length, 1);
  assert.equal(f.progress.card, null);
});
