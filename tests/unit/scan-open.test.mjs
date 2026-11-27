import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { qrcode } from '../../web/static/vendor/qrcode.mjs';
import { Element, dispatch } from './dom.mjs';

const app = new Element('div');
const dialogs = new Element('div');
globalThis.document = {
  getElementById: (id) => ({ app, 'dialog-root': dialogs })[id],
  createElement: (tag) => tag === 'template' ? {
    set innerHTML(markup) {
      this.content = { firstChild: Object.assign(new Element('svg'), { markup }) };
    },
  } : new Element(tag),
  documentElement: {},
  addEventListener() {},
};
globalThis.matchMedia = () => ({ matches: false });
globalThis.window = {};
globalThis.location = new URL('https://desk.example:18492/page?filter=online#details');

const { setLanguage } = await import('../../web/static/js/i18n.js');
const { state } = await import('../../web/static/js/state.js');
const { showLogin } = await import('../../web/static/js/views/login.js');
const { renderList } = await import('../../web/static/js/views/list.js');

const english = JSON.parse(readFileSync(new URL('../../web/static/i18n/en.json', import.meta.url), 'utf8'));

test('Scan to open comes before the menus on signed-out and signed-in pages', async (t) => {
  t.mock.method(globalThis, 'fetch', async () => Response.json(english));
  await setLanguage('en');

  for (const [me, draw, menus] of [
    [null, showLogin, ['btn text bar-btn lang-btn']],
    [{ display: 'Sean' }, renderList, ['btn text bar-btn lang-btn', 'btn text bar-btn name-btn']],
  ]) {
    state.me = me;
    draw();
    const header = app.firstElementChild.firstElementChild;
    const [, end] = header.childNodes;
    const [scan, ...dropdowns] = end.childNodes;
    assert.equal(end.className, 'topbar-end');
    assert.equal(scan.tagName, 'button');
    assert.equal(scan.getAttribute('aria-label'), english['scanOpen.title']);
    assert.deepEqual(dropdowns.map((el) => el.firstElementChild.className), menus);

    dispatch(scan, 'click');
    const [title, body, actions] = dialogs.firstElementChild.firstElementChild.childNodes;
    assert.deepEqual(title.childNodes, [english['scanOpen.title']]);
    const [qr, lead, address] = body.childNodes;
    const url = 'https://desk.example:18492/';
    const expected = qrcode(0, 'M');
    expected.addData(url);
    expected.make();
    assert.equal(qr.className, 'qr');
    assert.equal(qr.firstElementChild.markup, expected.createSvgTag({ cellSize: 1, scalable: true }));
    assert.deepEqual(lead.childNodes, [english['scanOpen.lead']]);
    assert.equal(address.firstElementChild.className, 'copyable-code');
    assert.deepEqual(address.firstElementChild.childNodes, [url]);

    assert.equal(actions.childNodes.length, 1);
    const close = actions.firstElementChild;
    assert.deepEqual(close.childNodes, [english['common.close']]);
    dispatch(close, 'click');
    assert.equal(dialogs.childNodes.length, 0);
  }
});
