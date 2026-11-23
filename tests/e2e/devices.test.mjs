// A new device (`c`, its own browser storage): approved and denied on a signed-in device,
// controlling the computer; my devices; renaming and removing a device; the passwords; a removed device let
// in again at the computer, by asking there and by its QR code; device checks off and on;
// removing a device at the computer. Changes the account's passwords and names and puts each
// back, so the areas after it sign in as before.
import { sleep, SCREENS, TAKE_OVER, signIn, superIn, nameThisDevice, t, q, fixed } from './harness.mjs';

export const area = {
  name: 'devices',
  needs: ['auth'],
  async run(h) {
    const { user, password, superPassword, url } = h.opts;
    const b = await h.secondPage();
    await h.freeComputer(b);
    await b.v(`d.show('detail', d.devices()[0].id)`);
    const c = await h.contextPage();
    await c.send('Page.navigate', { url });
    await c.send('Page.bringToFront');
    await c.addAuthenticator();
    await c.until(`!!document.querySelector('form.login')`, 'new browser login form');
    await signIn(c, { user, password });
    const cName = await nameThisDevice(c, '测试平板');
    await c.until(`!!document.querySelector('.code')`, 'new browser waits');
    const shownCode = await c.eval(`document.querySelector('.code').textContent`);
    await b.until(`document.querySelector('.dialog h2')?.textContent.includes(${JSON.stringify(fixed('approve.deviceTitle'))})`, 'approved browser is asked');
    h.check('an approved browser is asked, showing the same code and the name the new device was given', await b.eval(`document.querySelector('.dialog').textContent.includes(${JSON.stringify(shownCode)}) && document.querySelector('.dialog').textContent.includes(${JSON.stringify(cName)})`), shownCode);
    await b.shot('approve-browser', 1440, 900);
    await b.metrics(1440, 900);
    await b.clickText('.dialog button', t('common.deny'));
    await c.until(`document.querySelector('.error')?.textContent.includes(${q('wait.rejected')})`, 'new browser told it was denied');
    h.check('denying sends the new browser back to login', true);

    // The signed-in device sleeps meanwhile (a phone in a pocket): its socket is still open but
    // the network dropped it, so the request does not reach it. Back at the front after a while,
    // it is asked at once, without a reload.
    const visible = (on) => b.eval(`(() => {
      for (const k of ['hidden', 'visibilityState']) delete document[k];
      if (!${on}) { Object.defineProperty(document, 'hidden', { configurable: true, get: () => true }); Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' }); }
      document.dispatchEvent(new Event('visibilitychange'));
    })()`);
    const loadedAt = await b.eval(`performance.timeOrigin`);
    await visible(false);
    await b.v(`d.deafenSignaling()`);
    await signIn(c, { user, password });
    await c.until(`!!document.querySelector('.code')`, 'new browser waits again (its passkey is kept)');
    await sleep(1500);
    h.check('a sleeping device whose socket the network dropped hears nothing (the case being fixed)', await b.eval(`!document.querySelector('.dialog')`));
    await sleep(9000);
    await visible(true);
    await b.until(`document.querySelector('.dialog h2')?.textContent.includes(${JSON.stringify(fixed('approve.deviceTitle'))})`, 'asked once back at the front', 5000);
    h.check('back at the front after a while, the signed-in device is asked at once, without reloading', await b.eval(`performance.timeOrigin`) === loadedAt);
    await b.clickText('.dialog button', t('common.approve'), { exact: true });
    await c.until(`!!document.querySelector('.card')`, 'new browser signed in');
    h.check('approving signs the new device in', true);
    // Approved by a device the computer trusts: it controls the computer without being added there.
    await c.eval(`document.querySelector('.card-open').click()`);
    await c.until(SCREENS, 'new browser detail page');
    await c.eval(TAKE_OVER);
    await c.until(`!!s && s.info().connection === 'connected' && s.info().w > 0`, 'approved device connects');
    h.check('a device approved on another device controls the computer with no extra step', true);
    await c.v(`s.leave()`);

    // A laptop driven by its touchpad that also has a touch screen: the touch buttons show.
    await c.send('Page.addScriptToEvaluateOnNewDocument', { source: `Object.defineProperty(Navigator.prototype, 'maxTouchPoints', { get: () => 10 });` });
    await c.eval(`location.reload()`);
    await c.until(`!!document.querySelector('.card')`, 'touch-screen laptop: list');
    await c.eval(`localStorage.setItem('deskway.autofull', '0'); document.querySelector('.card-open').click()`);
    await c.until(SCREENS, 'touch-screen laptop: detail');
    await c.until(`d.devices()[0] && !d.devices()[0].controller`, 'touch-screen laptop: the computer is free', 30000);
    await c.eval(TAKE_OVER);
    await c.until(`!!s && s.info().connection === 'connected'`, 'touch-screen laptop: session');
    h.check('a laptop with a touch screen (mouse first) shows "Touchpad"/"Tap" and "Mouse" too', await c.v(`!matchMedia('(pointer: coarse)').matches && !!s.el.modeBtn && !!s.el.padBtn && s.el.toolbar.contains(s.el.padBtn)`), await c.v(`JSON.stringify({ coarse: matchMedia('(pointer: coarse)').matches, points: navigator.maxTouchPoints, mode: !!s.el.modeBtn, pad: !!s.el.padBtn })`));
    await c.v(`s.leave()`);

    const menu = (p, label) => p.eval(`(async () => {
      document.querySelector('.topbar .name-btn').click();
      await new Promise((r) => setTimeout(r, 50));
      [...document.querySelectorAll('.menu-item')].find((b) => b.textContent === ${JSON.stringify(label)}).click();
    })()`);
    // Account: a row's "Change" opens its dialog.
    const change = async (p, row) => {
      await menu(p, t('account.title'));
      await p.until(`!!document.querySelector('.page[data-page="account"] .setting-row')`, 'Account page');
      await p.eval(`[...document.querySelectorAll('.setting-row')].find((r) => r.firstElementChild.firstElementChild.textContent === ${JSON.stringify(row)}).querySelector('button').click()`);
      await p.until(`!!document.querySelector('.dialog.form')`, `${row} dialog`);
    };
    await b.v(`d.show('list')`);
    await menu(b, t('devices.title'));
    await b.until(`document.querySelectorAll('.device-row').length === 3`, 'device list: two controllers, one computer');
    await b.shot('devices-wide', 1440, 900);
    await b.shot('devices-portrait', 390, 844);
    await b.metrics(1440, 900);
    h.check('my devices lists the controllers and the computers each in their own card, this browser tagged This device', await b.eval(`(() => {
      const [controllers, computers] = [...document.querySelectorAll('.device-list.grouped')];
      return controllers.querySelectorAll('.device-row').length === 2 && computers.querySelectorAll('.device-row').length === 1 && [...controllers.querySelectorAll('.tag')].some((t) => t.textContent === ${q('devices.tagThis')});
    })()`));
    // The browser was asked to keep the device key when it was made; the page says so exactly
    // when it did not.
    const kept = await b.eval(`navigator.storage.persisted()`);
    h.check(`my devices: the key-may-be-deleted note shows only when storage is not kept (kept: ${kept})`, await b.eval(`!!document.querySelector('.key-hint')`) === (kept === false));
    // This device's picture: a menu of two, sharper by default; the choice is kept here.
    const pictureRow = `[...document.querySelectorAll('.setting-row')].find((r) => r.firstElementChild.firstElementChild.textContent === ${q('devices.picture')})`;
    h.check('my devices: Picture is Sharper by default, with what it means', await b.eval(`(() => { const r = ${pictureRow}; return !!r && r.querySelector('.btn').textContent === ${q('picture.sharp')} && r.textContent.includes(${q('picture.sharpSub')}); })()`));
    await b.eval(`${pictureRow}.querySelector('.btn').click()`);
    await b.eval(`[...document.querySelectorAll('.setting-row .menu-item')].find((x) => x.textContent === ${q('picture.smooth')}).click()`);
    await b.until(`(${pictureRow})?.querySelector('.btn').textContent === ${q('picture.smooth')} && localStorage.getItem('deskway.picture') === 'smooth'`, 'smoother chosen');
    h.check('my devices: Smoother is chosen from the menu and kept in this browser', true);
    await b.eval(`localStorage.removeItem('deskway.picture')`);
    // Renaming another device, under its ⋯ with its name filled in: the list shows the new name
    // at once, and the computer, which checks the signature, shows it too.
    const otherMenu = `[...document.querySelector('.device-list.grouped').querySelectorAll('.device-row')].find((r) => !r.textContent.includes(${q('devices.tagThis')})).querySelector('.device-actions .more')`;
    const hostPanel = h.panelPipe();
    await b.eval(`${otherMenu}.click()`);
    await b.clickText('.device-actions .menu-item', t('common.rename'), { exact: true });
    await b.until(`document.querySelector('.dialog.form input')?.value === ${JSON.stringify(cName)}`, 'rename form with the name filled in');
    await b.eval(`(() => { document.querySelector('.dialog.form input').value = '新加坡办公室 PC'; document.querySelector('.dialog.form button[type=submit]').click(); })()`);
    await b.until(`[...document.querySelectorAll('.device-row .name')].some((n) => n.textContent === '新加坡办公室 PC')`, 'renamed in the list');
    let onHost = false;
    for (let i = 0; i < 25 && !onHost; i++) { await sleep(200); onHost = !!hostPanel.statuses.at(-1)?.devices?.some((x) => x.name === '新加坡办公室 PC'); }
    hostPanel.close();
    h.check('renaming another device: the list and the computer show the new name', onHost, JSON.stringify(hostPanel.statuses.at(-1)?.devices?.map((x) => x.name)));
    h.check('the computer knows the account it belongs to, for its panel', hostPanel.statuses.at(-1)?.account?.user === user, JSON.stringify(hostPanel.statuses.at(-1)?.account));
    const removeOther = async () => {
      await b.v(`d.show('devices')`);
      await b.until(`document.querySelectorAll('.device-row').length === 3`, 'device list');
      await b.eval(`[...document.querySelector('.device-list.grouped').querySelectorAll('.device-row')].find((r) => !r.textContent.includes(${q('devices.tagThis')})).querySelector('.device-actions .more').click()`);
      await b.clickText('.device-actions .menu-item', t('common.remove'), { exact: true });
      await b.until(`document.querySelector('.dialog h2')?.textContent.includes(${JSON.stringify(fixed('devices.removeTitle'))})`, 'remove asks first');
      await b.clickText('.dialog button', t('common.remove'), { exact: true });
      await b.until(`document.querySelectorAll('.device-row').length === 2`, 'row removed');
      await b.v(`d.show('list')`);
    };
    await removeOther();
    h.check('removing a device takes it off the list', true);

    // ---- changing the password and the super password ----
    const fillForm = (values) => b.eval(`(() => { const f = [...document.querySelectorAll('.dialog.form input[type=password]')]; ${JSON.stringify(values)}.forEach((v, i) => { f[i].value = v; }); document.querySelector('.dialog.form button[type=submit]').click(); })()`);
    const checkboxLine = `(() => {
      const l = document.querySelector('.dialog.form label.check'), box = l.querySelector('input').getBoundingClientRect(), field = document.querySelector('.dialog.form input[type=password]').getBoundingClientRect();
      const text = [...l.childNodes].find((n) => n.nodeType === 3); const tr = document.createRange(); tr.selectNodeContents(text); const t = tr.getBoundingClientRect();
      return Math.abs(box.left - field.left) < 2 && t.left > box.right && Math.abs((t.top + t.bottom) / 2 - (box.top + box.bottom) / 2) < 4 && tr.getClientRects().length === 1;
    })()`;
    await change(b, t('account.password'));
    h.check('change password: the checkbox and its words sit on one line, from the left', await b.eval(checkboxLine));
    await b.metrics(390, 844, true);
    await sleep(150);
    h.check('…also on a phone held upright', await b.eval(checkboxLine));
    await b.shot('change-password-phone', 390, 844);
    await b.metrics(1440, 900);
    await fillForm(['wrong current password', 'brand new password 1', 'brand new password 1']);
    await b.until(`document.querySelector('.dialog.form .error')?.textContent.includes(${q('pw.wrongCurrent')})`, 'wrong current password message');
    h.check('change password: a wrong current password is refused and the page stays signed in', await b.eval(`!document.querySelector('form.login') && !!document.querySelector('.dialog.form')`));
    await fillForm([password, 'brand new password 1', 'different']);
    await b.until(`document.querySelector('.dialog.form .error')?.textContent.includes(${q('pw.mismatch')})`, 'mismatch message');
    h.check('change password: the two new passwords must match', true);
    await fillForm([password, 'brand new password 1', 'brand new password 1']);
    await b.until(`document.querySelector('.dialog h2')?.textContent === ${q('pw.doneTitle')}`, 'password changed');
    await b.clickText('.dialog button', t('common.gotIt'));
    // And back, so the rest of the run signs in as before.
    await change(b, t('account.password'));
    await fillForm(['brand new password 1', password, password]);
    await b.until(`document.querySelector('.dialog h2')?.textContent === ${q('pw.doneTitle')}`, 'password changed back');
    await b.clickText('.dialog button', t('common.gotIt'));
    h.check('change password: the new password is required next time, and it can be changed back', true);
    await change(b, t('account.super'));
    await fillForm(['wrong account password', superPassword, superPassword]);
    await b.until(`document.querySelector('.dialog.form .error')?.textContent.includes(${q('super.wrongAccount')})`, 'wrong account password message');
    await fillForm([password, superPassword, superPassword]);
    await b.until(`document.querySelector('.dialog h2')?.textContent === ${q('super.doneTitle')}`, 'super password changed');
    await b.clickText('.dialog button', t('common.gotIt'));
    h.check('change super password: needs the account password', true);

    // ---- the display name and the sign-in name ----
    const display = await b.v(`d.me().display`);
    const fill = (name, pw) => b.eval(`(() => { const f = document.querySelector('.dialog.form'); f.querySelector('input:not([type=password])').value = ${JSON.stringify(name)}; const p = f.querySelector('input[type=password]'); if (p) p.value = ${JSON.stringify(pw || '')}; f.querySelector('button[type=submit]').click(); })()`);
    await change(b, t('account.display'));
    h.check('Display name: changed in a dialog like the passwords, not in the top bar', await b.eval(`!document.querySelector('.topbar input')`));
    await fill('小林');
    await b.until(`!document.querySelector('.dialog') && document.querySelector('.topbar .name-btn').textContent === '小林'`, 'display name saved');
    h.check('Display name: the top bar and the Account page show it at once', await b.eval(`[...document.querySelectorAll('.setting-row')].some((r) => r.textContent.includes('小林'))`));
    await change(b, t('account.display'));
    await fill(display);
    await b.until(`!document.querySelector('.dialog')`, 'display name back');
    await change(b, t('account.name'));
    await b.shot('change-sign-in-name', 390, 844);
    await b.metrics(1440, 900);
    await fill('小林', password);
    await b.until(`document.querySelector('.dialog.form .error')?.textContent.includes(${q('signin.rule')})`, 'sign-in name rule');
    h.check('Sign-in name: a name outside the rule is refused with the rule', true);
    await fill('tester-renamed', 'wrong password');
    await b.until(`document.querySelector('.dialog.form .error')?.textContent.includes(${q('pw.wrongCurrent')})`, 'sign-in name wrong password');
    h.check('Sign-in name: needs the current password', true);
    await fill('tester-renamed', password);
    await b.until(`document.querySelector('.dialog h2')?.textContent === ${q('signin.doneTitle')}`, 'sign-in name changed');
    h.check('Sign-in name: changed; the dialog says what to sign in with', await b.eval(`document.querySelector('.dialog p').textContent.includes('tester-renamed')`));
    await b.clickText('.dialog button', t('common.gotIt'));
    h.check('Sign-in name: the page stays signed in, under the new name', await b.v(`d.me().user === 'tester-renamed' && !document.querySelector('form.login')`));
    await change(b, t('account.name'));
    await fill(user, password);
    await b.until(`document.querySelector('.dialog h2')?.textContent === ${q('signin.doneTitle')}`, 'sign-in name back');
    await b.clickText('.dialog button', t('common.gotIt'));

    await c.eval(`location.reload()`);
    await sleep(800);
    await c.until(`!!document.querySelector('form.login')`, 'removed browser signed out');
    h.check('removing a browser signs it out at once', true);
    await signIn(c, { user, password });
    await c.until(`!!document.querySelector('.code')`, 'removed browser needs approval');
    h.check('a removed browser needs approval again', true);
    await b.until(`document.querySelector('.dialog h2')?.textContent.includes(${JSON.stringify(fixed('approve.deviceTitle'))})`, 'asked after removal');
    await b.clickText('.dialog button', t('common.deny'));

    // The removed device gets back in with the super password, but the computer heard about the
    // removal: it must be let in at the computer again. This time by asking the person there.
    const inWithSuper = async (label) => {
      await signIn(c, { user, password });
      await c.until(`!!document.querySelector('.wait-step')`, label);
      await superIn(c, superPassword);
      await c.until(`!!document.querySelector('.card')`, `${label}: in with the super password`);
    };
    await c.until(`document.querySelector('.error')?.textContent.includes(${q('wait.rejected')})`, 'removed browser denied');
    await inWithSuper('waiting again');
    await c.eval(`document.querySelector('.card-open').click()`);
    await c.until(SCREENS, 'detail after super password');
    // Let the browser enter full screen before delivering the real refusal.
    await c.v(`d.interceptSignals((m) => {
      if (m.type !== 'bye' || m.reason !== 'unpaired') return m;
      window.__unpairedReply = m;
      return { type: 'ignored' };
    })`);
    await c.send('Page.bringToFront');
    await c.eval(`localStorage.setItem('deskway.autofull', '1')`);
    await c.send('Runtime.evaluate', { expression: TAKE_OVER, userGesture: true });
    await c.until(`window.__unpairedReply && document.fullscreenElement?.contains(document.getElementById('dialog-root'))`, 'full screen before the refusal');
    await c.v(`(d.interceptSignals(null), d.serverSays(window.__unpairedReply))`);
    await c.eval(`localStorage.setItem('deskway.autofull', '0')`);
    await c.until(`document.querySelector('.dialog h2')?.textContent.includes(${JSON.stringify(fixed('notAdded.title'))})`, 'removed device refused by the computer');
    h.check('a removed device returns from full screen and can request control', await c.eval(`!document.fullscreenElement && !document.querySelector('.session') && document.querySelector('.dialog .primary')?.textContent === ${q('notAdded.request')}`));
    const panel = h.panelPipe();
    await c.clickText('.dialog button', t('notAdded.request'), { exact: true });
    await c.until(`document.querySelector('.dialog h2')?.textContent.includes(${JSON.stringify(fixed('ask.waitTitle'))})`, 'waiting for the person at the computer');
    const askedCode = await c.eval(`document.querySelector('.dialog .code').textContent.replace(/\\s/g, '')`);
    let ask = null;
    for (let i = 0; i < 50 && !ask; i++) {
      await sleep(200);
      ask = panel.statuses.flatMap((st) => st.asks || []).find((x) => x.code === askedCode) || null;
    }
    h.check('the computer\'s panel shows the request with the same digits', !!ask, askedCode);
    await c.shot('ask-portrait', 390, 844);
    await c.metrics(1440, 900);
    if (ask) panel.send({ AnswerAsk: [ask.session, true] });
    await c.until(`!!s && s.info().connection === 'connected' && s.info().w > 0`, 'allowed at the computer, then connected');
    h.check('allowing at the computer lets the device in and connects', true);
    panel.close();
    await c.v(`s.leave()`);

    // Removed once more, then added back with the computer's QR code instead.
    await removeOther();
    await c.eval(`location.reload()`);
    await sleep(800);
    await c.until(`!!document.querySelector('form.login')`, 'signed out again');
    await inWithSuper('waiting once more');
    const pairingCode = h.pairCode();
    await c.eval(`location.href = ${JSON.stringify(url)} + '/#pair=' + ${JSON.stringify(h.hostId)} + '.' + ${JSON.stringify(pairingCode.replace(/-/g, ''))}`);
    await c.eval(`location.reload()`);
    await sleep(800);
    await c.until(`document.querySelector('.dialog h2')?.textContent.includes(${JSON.stringify(fixed('qr.title'))})`, 'add-from-QR dialog');
    h.check('the QR link leaves no code in the address bar', await c.eval(`!location.hash`));
    await c.shot('qr-add-portrait', 390, 844);
    await c.metrics(1440, 900);
    await c.clickText('.dialog button', t('qr.add'), { exact: true });
    await c.until(`document.querySelector('.dialog h2')?.textContent === ${q('approve.addedTitle')}`, 'added with the QR code');
    await c.clickText('.dialog button', t('detail.takeOver'), { exact: true });
    await c.until(`!!s && s.info().connection === 'connected' && s.info().w > 0`, 'connected after the QR code');
    h.check('scanning the computer\'s QR code adds the device, with no typing', true);
    await c.v(`s.leave()`);

    // Device checks can be turned off (account password only), and on again, from settings.
    // Its switch, by its name: a computer's AI switches are settings' rows too.
    const checks = `[...document.querySelectorAll('.setting-row')].find((r) => r.firstElementChild.firstElementChild.textContent === ${q('devices.checks')})?.querySelector('.switch')`;
    await b.v(`d.show('devices')`);
    await b.until(`!!(${checks})`, 'device check switch');
    h.check('device checks are on by default', await b.eval(`(${checks}).getAttribute('aria-checked') === 'true'`));
    await b.eval(`(${checks}).click()`);
    await b.until(`document.querySelector('.dialog h2')?.textContent.includes(${q('devices.offTitle')})`, 'turning off asks first');
    await b.clickText('.dialog button', t('devices.turnOff'), { exact: true });
    await b.until(`(${checks})?.getAttribute('aria-checked') === 'false'`, 'checks off');
    h.check('device checks can be turned off in settings', true);
    await b.eval(`(${checks}).click()`);
    await b.until(`(${checks})?.getAttribute('aria-checked') === 'true'`, 'checks on again');
    h.check('and turned back on', true);
    await b.v(`d.show('list')`);

    // At the computer: remove one device. The count drops at once and that device is refused.
    const panel2 = h.panelPipe();
    await sleep(500);
    const before = panel2.statuses.at(-1)?.paired ?? 0;
    const cCred = await c.eval(`JSON.parse(localStorage.getItem('deskway.passkey')).cred`);
    panel2.send({ RemoveDevices: [cCred] });
    let after = before;
    for (let i = 0; i < 25 && after === before; i++) { await sleep(200); after = panel2.statuses.at(-1)?.paired ?? before; }
    h.check('removing one device at the computer updates the count at once', after === before - 1, `${before} -> ${after}`);
    panel2.close();
    await c.v(`d.show('detail', d.devices()[0].id)`);
    await c.until(`[...document.querySelectorAll('.display-tile, .preview.clickable')].length > 0`, 'detail again');
    await c.v(`d.press(d.devices()[0].id)`);
    await c.until(`document.querySelector('.dialog h2')?.textContent.includes(${JSON.stringify(fixed('notAdded.title'))})`, 'removed at the computer, refused');
    h.check('a device removed at the computer can no longer control it', true);
    await c.eval(`document.querySelector('.dialog button').click()`);
  },
};
