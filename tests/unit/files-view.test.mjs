import { test } from 'node:test';
import assert from 'node:assert/strict';
import { crumbs, decodeName, displayName, entryActions, permissions } from '../../web/static/js/session/file-listing.js';

const key = (name) => Buffer.from(name).toString('base64url');

globalThis.document = { getElementById: () => null };
globalThis.matchMedia = () => ({ matches: false });
const { FilesView } = await import('../../web/static/js/session/files-view.js');

function filesView() {
  const entries = [];
  const view = Object.assign(Object.create(FilesView.prototype), {
    el: { hidden: false },
    list: {},
    observer: { disconnect() {} },
    rows: {
      append: (...rows) => entries.push(...rows),
      replaceChildren: () => { entries.length = 0; },
      get childElementCount() { return entries.length; },
    },
    drawPath() {},
    showNotice() {},
    update() {},
    row: ({ name }) => displayName(name),
  });
  return { view, entries };
}

test('a listing keeps the returned path and appends the next page using its cursor', async () => {
  const path = [key('home')];
  const messages = [];
  const { view, entries } = filesView();
  view.request = async (message, reply) => {
    assert.equal(reply, 'listing');
    messages.push(message);
    return message.cursor ? { path, entries: [{ name: key('b') }] }
      : { path, entries: [{ name: key('a') }], cursor: 'next' };
  };

  await view.load();
  await view.load(view.cursor);
  assert.deepEqual(messages, [
    { t: 'list', path: undefined, cursor: undefined },
    { t: 'list', path, cursor: 'next' },
  ]);
  assert.deepEqual(entries, ['a', 'b']);
  assert.equal(view.cursor, undefined);
});

test('Delete submitted during an automatic page waits for it and refreshes before navigation', async () => {
  const { view } = filesView();
  const path = [key('home')];
  const messages = [];
  view.s = { connected: true };
  view.path = path;
  view.cursor = 'next';
  view.channel = { readyState: 'open', send: (data) => messages.push(JSON.parse(data)) };
  const flush = () => new Promise(setImmediate);
  const listing = (path, cursor) => view.receive(JSON.stringify({ t: 'listing', path, entries: [], cursor }));
  const entry = key('file');

  const page = view.next();
  await flush();
  const deletion = view.change({ t: 'delete', path, entry });
  const navigation = view.open([]);
  assert.deepEqual(messages, [{ t: 'list', path, cursor: 'next' }]);
  listing(path, 'another-page');
  await page;
  await flush();
  assert.deepEqual(messages.at(-1), { t: 'delete', path, entry });
  view.receive('{"t":"changed"}');
  await flush();
  assert.deepEqual(messages.at(-1), { t: 'list', path });
  listing(path);
  await deletion;
  await flush();
  assert.deepEqual(messages.at(-1), { t: 'list', path: [] });
  listing([]);
  await navigation;
  assert.equal(messages.length, 4);
});

test('navigation drops an automatic page that has yet to start', async () => {
  const { view } = filesView();
  const messages = [];
  view.s = { connected: true };
  view.channel = { readyState: 'open' };
  view.cursor = 'next';
  view.request = async (message) => {
    messages.push(message);
    return { path: [], entries: [] };
  };
  view.next();
  await view.open([]);
  assert.deepEqual(messages, [{ t: 'list', path: [], cursor: undefined }]);
});

test('file names escape controls and preserve the encoded name for operations', () => {
  for (const [name, visible] of [
    ['中文', '中文'],
    ['  a  b  ', '  a  b  '],
    ['log\n\u202ereport', 'log\\u{a}\\u{202e}report'],
    ['a\\xff', 'a\\\\xff'],
    ['<img src=x>', '<img src=x>'],
    [Buffer.from([97, 255]), 'a\\xff'],
  ]) {
    assert.equal(displayName(key(name)), visible);
    if (typeof name === 'string') assert.equal(decodeName(key(name)).value, name);
    else assert.equal(decodeName(key(name)).value, undefined);
  }
});

test('breadcrumbs use encoded prefixes from the root through the current folder', () => {
  const home = key('home');
  const raw = key(Buffer.from([255]));
  assert.deepEqual(crumbs([]), [{ label: '/', path: [] }]);
  assert.deepEqual(crumbs([home, raw]), [
    { label: '/', path: [] },
    { label: 'home', path: [home] },
    { label: '\\xff', path: [home, raw] },
  ]);
});

test('entry menus and permissions use the entry kind and the host’s mode', () => {
  for (const [kind, opens, mode, expected, actions] of [
    ['folder', undefined, 0o1777, 'drwxrwxrwt', ['open', 'rename', 'delete']],
    ['file', undefined, 0o4640, '-rwSr-----', ['rename', 'delete']],
    ['link', true, 0o777, 'lrwxrwxrwx', ['open', 'rename', 'delete']],
    ['link', false, 0o777, 'lrwxrwxrwx', ['rename', 'delete']],
    ['special', undefined, 0o600, '?rw-------', ['rename', 'delete']],
  ]) {
    assert.deepEqual(entryActions({ kind, opens }), actions);
    assert.equal(permissions({ kind, mode }), expected);
  }
});
