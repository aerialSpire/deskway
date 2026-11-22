// Managing a computer: its menu from holding its card, a right-click and its ⋯ (on the card and
// on its page); its page's title row; renaming it, then removing it from the account. Last:
// removing it ends the test host's place there, so it runs after every other area.
import { sleep, click, centre, t, q, fixed } from './harness.mjs';

// The open menu's items, or null when no menu is open.
const OPEN_MENU = `(() => { const m = [...document.querySelectorAll('.page .menu')].find((x) => !x.hidden); return m ? [...m.querySelectorAll('.menu-item')].map((x) => x.textContent) : null; })()`;
const COMPUTER_MENU = JSON.stringify([t('common.rename'), t('computer.removeFromAccount')]);
const pick = (text) => `[...document.querySelectorAll('.page .menu:not([hidden]) .menu-item')].find((x) => x.textContent === ${JSON.stringify(text)}).click()`;

// The title row of a computer's page: where the way back, the name, the ⋯ and the status are.
const TITLE_ROW = `(() => {
  const r = (s) => document.querySelector(s)?.getBoundingClientRect();
  const back = r('.heading .title-back'), name = r('.heading h1'), more = r('.heading .more'), status = r('.heading > .status');
  const page = document.querySelector('.page');
  return {
    back: back && [back.left, back.top, back.bottom], name: [name.left, name.top, name.bottom],
    more: more && [more.left, more.right, more.top], status: status && status.left,
    edge: page.getBoundingClientRect().right - parseFloat(getComputedStyle(page).paddingRight),
    buttons: document.querySelectorAll('.heading .btn:not(.more)').length,
  };
})()`;

export const area = {
  name: 'manage',
  needs: ['auth'],
  async run(h) {
    const b = await h.secondPage();
    await h.freeComputer();
    // The first device watches the list, where the new name must show.
    await h.a.v(`d.show('list')`);
    // In front, so the page's timers run on time (background pages have theirs delayed).
    await b.send('Page.bringToFront');
    await b.metrics(1440, 900);
    await b.eval(`document.querySelector('.dialog button')?.click()`);
    await b.v(`d.show('list')`);
    await b.until(`!!document.querySelector('.card')`, 'computer list');
    const menuNow = async () => JSON.stringify(await b.eval(OPEN_MENU));
    // A real click, then a moment for the page to answer it.
    const clickAt = async (at, button) => { await click(b, at, button); await sleep(200); };

    // Holding a card (with a real mouse button), and right-clicking it.
    const cardAt = await centre(b, '.card .thumb');
    const mouse = (type) => b.send('Input.dispatchMouseEvent', { type, x: cardAt[0], y: cardAt[1], button: 'left', clickCount: 1 });
    await mouse('mousePressed');
    await b.until(`!!${OPEN_MENU}`, 'menu while held', 3000);
    await mouse('mouseReleased');
    await sleep(200);
    h.check('holding a computer opens its menu at its ⋯, and does not open the computer', await b.v(`d.view() === 'list'`) && await menuNow() === COMPUTER_MENU, await menuNow());
    await b.shot('computer-menu', 1440, 900);
    await b.metrics(1440, 900);
    await b.v(`d.serverSays({ type: 'devices', devices: d.devices() })`);
    await sleep(100);
    h.check('the server telling of a change does not close the open menu', await menuNow() === COMPUTER_MENU, await menuNow());
    await b.eval(pick(t('computer.removeFromAccount')));
    h.check('removing from the menu asks again first', await b.eval(`document.querySelector('.dialog h2').textContent.includes(${JSON.stringify(fixed('computer.removeTitle'))}) && [...document.querySelectorAll('.dialog button')].some((x) => x.textContent === ${q('common.cancel')})`));
    await b.clickText('.dialog button', t('common.cancel'));
    h.check('cancelling keeps the computer', await b.v(`d.devices().length === 1 && !document.querySelector('.dialog')`));
    await clickAt(cardAt, 'right');
    h.check('right-clicking a computer opens the same menu', await menuNow() === COMPUTER_MENU, await menuNow());
    await b.eval(pick(t('common.rename')));
    h.check('Rename opens the rename form', await b.eval(`!!document.querySelector('.dialog.form input')`));
    await b.clickText('.dialog button', t('common.cancel'));
    await clickAt(await centre(b, '.card .more'));
    h.check("the card's ⋯ opens the same menu, and does not open the computer", await b.v(`d.view() === 'list'`) && await menuNow() === COMPUTER_MENU, await menuNow());
    await b.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
    await b.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
    h.check('Esc closes the menu', await b.eval(`${OPEN_MENU} === null`));
    await clickAt(cardAt);
    await b.until(`d.view() === 'detail'`, 'a short click opens the computer', 3000);
    h.check('a short click anywhere on the card still opens the computer', true);

    // Its page: the way back, the name and the ⋯ on one row; the status under the name.
    await b.until(`!!document.querySelector('.heading .more')`, "the computer's page");
    const before = await b.eval(TITLE_ROW);
    h.check('its page: the way back starts the title row, the status starts where it does, the ⋯ ends the row; no other buttons',
      before.back && Math.abs(before.status - before.back[0]) <= 1 && before.back[1] >= before.name[1] && before.back[2] <= before.name[2]
        && Math.abs(before.more[1] - before.edge - 12) <= 1 && before.buttons === 0, JSON.stringify(before));
    await b.v(`d.serverSays({ type: 'devices', devices: d.devices().map((x) => ({ ...x, controller: { device: false, name: '小林的 Android 手机上的 Chrome 浏览器（一个很长很长的设备名称）', connecting: false } })) })`);
    await sleep(150);
    const busy = await b.eval(TITLE_ROW);
    h.check('its page: a long status wraps under the title and moves nothing on the title row', JSON.stringify([busy.back, busy.more, busy.status]) === JSON.stringify([before.back, before.more, before.status]), JSON.stringify(busy));
    await b.shot('detail-controlled', 1440, 900);
    await b.shot('detail-controlled-portrait', 390, 844);
    await b.metrics(1440, 900);
    await b.v(`d.serverSays({ type: 'devices', devices: d.devices().map((x) => ({ ...x, controller: undefined })) })`);
    await sleep(150);
    await clickAt(await b.eval(`(() => {
      const name = document.querySelector('.heading .title-text').firstChild;
      const first = document.createRange();
      first.setStart(name, 0);
      first.setEnd(name, 1);
      const r = first.getBoundingClientRect();
      return [r.left + r.width / 2, r.top + r.height / 2];
    })()`));
    h.check('its page: clicking even the first character of the name stays on the detail page', await b.v(`d.view() === 'detail'`));
    h.check('its page: one line says pressing a screen takes over, and nothing is laid over the screens', await b.eval(`document.querySelector('.preview .preview-hint')?.textContent === ${q('detail.hint')} && ![...document.querySelectorAll('.display-tile *')].some((x) => /take (control|over)/i.test(x.textContent))`));

    // A press whose lift never came (a session ended under the finger) does not freeze the page:
    // the computer goes offline and back, and its picture can be pressed again within moments.
    await b.eval(`document.dispatchEvent(new PointerEvent('pointerdown', { pointerId: 77, bubbles: true }))`);
    await b.v(`d.serverSays({ type: 'devices', devices: d.devices().map((x) => ({ ...x, online: false })) })`);
    await b.until(`!document.querySelector('.preview.clickable, .display-tile')`, 'shown offline');
    await b.v(`d.serverSays({ type: 'devices', devices: d.devices().map((x) => ({ ...x, online: true })) })`);
    await b.until(`!!document.querySelector('.preview.clickable, .display-tile')`, 'its picture can be pressed again', 4000);
    h.check('a lift that never came holds the page at most a moment: back online, the picture can be pressed', true);
    // Another device in control: pressing the picture says which, and asks.
    await b.v(`d.serverSays({ type: 'devices', devices: d.devices().map((x) => ({ ...x, controller: { device: false, name: '小林的手机', connecting: false } })) })`);
    await b.until(`!!document.querySelector('.preview.clickable, .display-tile')`, 'picture');
    await b.eval(`document.querySelector('.preview.clickable, .display-tile').click()`);
    await b.until(`!!document.querySelector('.dialog h2')`, 'asked first');
    h.check('pressing the picture of a computer someone controls asks first, naming them', await b.eval(`document.querySelector('.dialog h2').textContent.includes('小林的手机') && [...document.querySelectorAll('.dialog button')].some((x) => x.textContent === ${q('force.accountGo')})`), await b.eval(`document.querySelector('.dialog')?.textContent`));
    await b.clickText('.dialog button', t('common.cancel'));
    await b.v(`d.serverSays({ type: 'devices', devices: d.devices().map((x) => ({ ...x, controller: undefined })) })`);

    // The ⋯ with a real click at its place on the page.
    const moreAt = await centre(b, '.heading .more');
    h.check('its ⋯ is not covered by anything', await b.eval(`!!document.elementFromPoint(${moreAt[0]}, ${moreAt[1]})?.closest('.heading .more')`), await b.eval(`document.elementFromPoint(${moreAt[0]}, ${moreAt[1]})?.outerHTML.slice(0, 120)`));
    await clickAt(moreAt);
    h.check('a real click on its ⋯ opens the same menu, on top of the page', await menuNow() === COMPUTER_MENU
      && await b.eval(`(() => { const i = document.querySelector('.page .menu:not([hidden]) .menu-item').getBoundingClientRect(); return !!document.elementFromPoint(i.left + i.width / 2, i.top + i.height / 2)?.closest('.menu'); })()`), await menuNow());
    await b.shot('manage-portrait', 390, 844);
    await b.metrics(1440, 900);
    await b.eval(pick(t('common.rename')));
    h.check('Rename from the ⋯ is a plain rename form, nothing else in it', await b.eval(`!!document.querySelector('.dialog.form input') && [...document.querySelectorAll('.dialog.form button')].map((x) => x.textContent).join() === ${JSON.stringify([t('common.cancel'), t('common.save')].join())}`), await b.eval(`[...document.querySelectorAll('.dialog button')].map((x) => x.textContent).join()`));
    await b.eval(`(() => { document.querySelector('.dialog.form input').value = '书房电脑'; document.querySelector('.dialog.form button[type=submit]').click(); })()`);
    await h.a.until(`d.devices()[0] && d.devices()[0].name === '书房电脑'`, 'new name reaches other devices');
    h.check('renaming a computer shows the new name on every device', true);

    // Only the mark goes back.
    await clickAt(await centre(b, '.heading .title-back'));
    h.check('its page: the mark before the name goes back to My computers', await b.v(`d.view() === 'list'`));
    await b.eval(`document.querySelector('.card-open').click()`);
    await b.until(`!!document.querySelector('.heading .more')`, "the computer's page again");
    await b.eval(`document.querySelector('.heading .more').click()`);
    await b.until(`!!${OPEN_MENU}`, 'menu again');
    await b.eval(pick(t('computer.removeFromAccount')));
    await b.until(`document.querySelector('.dialog h2')?.textContent.includes(${JSON.stringify(fixed('computer.removeTitle'))})`, 'remove asks first');
    await b.clickText('.dialog button', t('common.remove'), { exact: true });
    await b.until(`d.view() === 'list' && d.devices().length === 0`, 'computer gone');
    await h.a.until(`d.devices().length === 0`, 'computer gone everywhere', 5000).catch(() => {});
    h.check('removing a computer takes it off the account', await h.a.v(`d.devices().length === 0`));
  },
};
