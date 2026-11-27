// Signing in a new browser: a wrong password, naming it, the waiting screen with its code, account recovery,
// approving the new computer; then the list with its thumbnail. Every other area
// needs what it leaves: `h.a`, the account's first device, signed in on the list, and
// `h.hostId`, the test computer.
import { sleep, signIn, issueAdminLink, redeemSetup, nameThisDevice, t, q } from './harness.mjs';

export const area = {
  name: 'auth',
  needs: [],
  async run(h) {
    const { url, user, password, setupLink, serverExe, serverState } = h.opts;
    const a = await h.newPage();
    h.a = a;
    await a.addAuthenticator();
    await sleep(500);
    await a.until(`!!document.querySelector('form.login')`, 'login form');
    await a.eval(`localStorage.setItem('deskway.autofull', '0')`);
    await a.shot('login', 1280, 800);
    await signIn(a, { user, password: 'wrong-password' });
    await a.until(`document.querySelector('.error')?.textContent.includes(${q('login.wrong')})`, 'wrong password message');
    h.check('wrong password is rejected', true);

    await signIn(a, { user, password });
    await a.until(`!!document.querySelector('.name-step input')`, 'the new device asks for its name');
    h.check('a new device is named first, the name of its kind filled in, before any code', await a.eval(`document.querySelector('.name-step input').value.length > 0 && !document.querySelector('.code')`));
    await nameThisDevice(a);
    await a.until(`!!document.querySelector('.wait-step .code')`, 'waiting screen with its code');
    h.check('a new device shows its six digits at once: no passkey, no fingerprint, no button', await a.eval(`!document.querySelector('.wait-step button') && /^\\d{3} \\d{3}$/.test(document.querySelector('.code').textContent)`));
    await a.shot('waiting-wide', 1440, 900);
    await a.shot('waiting-portrait', 390, 844);
    await a.metrics(1440, 900);
    const link = setupLink || await issueAdminLink(serverExe, serverState, url);
    await redeemSetup(a, link, { password });
    await a.until(`document.querySelector('.dialog h2')?.textContent.includes(${q('approve.computerTitle')})`, 'computer approval dialog');
    h.check('the account link signs in, and the new computer asks for approval', await a.eval(`!document.querySelector('.card')`));
    await a.shot('approve-computer', 1440, 900);
    await a.clickText('.dialog button', t('common.approve'));
    await a.until(`!!document.querySelector('.card')`, 'device card');
    h.check('the computer appears after approval', true);
    h.check('login shows the device', await a.eval(`document.querySelector('.card-open').textContent.length > 0`));
    await a.until(`!!document.querySelector('img[data-thumb]:not([hidden])')`, 'thumbnail', 12000);
    h.check('thumbnail arrives', true, `${await a.eval(`document.querySelector('img[data-thumb]').naturalWidth`)}px wide`);
    await a.shot('overview-wide', 1440, 900);
    await a.shot('overview-portrait', 390, 844);
    await a.shot('overview-landscape', 844, 390);
    await a.metrics(1440, 900);
    h.hostId = await a.v(`d.devices()[0].id`);
  },
};
