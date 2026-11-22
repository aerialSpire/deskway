// The relay alone carries a session: the browser may only use relay candidates, and the host
// (started with DESKWAY_TEST_RELAY) offers only its relay. Opt-in, and run alone
// (`--areas relay`): it needs a fresh member account, made by the admin (POST /api/members) and
// never signed in, given as --user with its initial password, and a host enrolled with that
// password and started for it. Its page has its own storage, as a new device: let in by the
// initial password, it chooses its own passwords first.
import { sleep, SCREENS, TAKE_OVER, signIn, t, q } from './harness.mjs';

export const area = {
  name: 'relay',
  needs: [],
  async run(h) {
    const { url, user, password } = h.opts;
    const p = await h.contextPage();
    // Forces the browser side onto the relay, whatever the page asks for.
    await p.send('Page.addScriptToEvaluateOnNewDocument', { source: `{ const P = RTCPeerConnection; window.RTCPeerConnection = function (c) { return new P({ ...c, iceTransportPolicy: 'relay' }); }; window.RTCPeerConnection.prototype = P.prototype; }` });
    await p.send('Page.navigate', { url });
    await p.until(`!!document.querySelector('form.login')`, 'login form');
    await signIn(p, { user, password });
    await p.until(`!!document.querySelector('.page[data-page="choose"]')`, 'choosing its passwords');
    await p.eval(`(() => { const ins = document.querySelectorAll('.choose input'); ['relay own password', 'relay own password', 'relay super password', 'relay super password'].forEach((v, i) => { ins[i].value = v; }); document.querySelector('.choose button[type="submit"]').click(); })()`);
    await p.until(`document.querySelector('.dialog h2')?.textContent.includes(${q('approve.computerTitle')})`, 'computer approval');
    await p.clickText('.dialog button', t('common.approve'));
    await p.until(`!!document.querySelector('.card')`, 'card');
    await p.until(`document.querySelector('.card .status')?.textContent.includes(${q('status.online')})`, 'computer online');
    await p.eval(`document.querySelector('.card-open').click()`);
    await p.until(SCREENS, 'detail');
    const t0 = Date.now();
    await p.eval(TAKE_OVER);
    await p.until(`!!s && s.info().connection === 'connected' && s.info().w > 0`, 'video over the relay', 20000);
    h.check('video over the relay', true, `first frame in ${Date.now() - t0} ms`);
    await sleep(6000);
    const st = await p.v(`s.stats()`);
    console.log(`      ${JSON.stringify(st)}`);
    h.check('the browser really used a relay', st.local === 'relay');
    h.check('the host really used a relay', st.remote === 'relay');
    h.check('frames keep decoding', st.decoded > 0);
    h.check('the input channel opens over the relay', st.input === 'open');
  },
};
