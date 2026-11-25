// The relay alone carries a session: the browser may only use relay candidates, and the host
// (started with DESKWAY_TEST_RELAY) offers only relays. For TLS, silently drop the
// host's traffic to the server's UDP and TCP TURN port. Opt-in, and run alone
// (`--areas relay`): it needs a fresh member account, made by the admin (POST /api/members) and
// never signed in, given as --user with its initial password, and a host enrolled with that
// password and started for it. Its page has its own storage, as a new device: let in by the
// initial password, it chooses its own passwords first.
import { sleep, SCREENS, TAKE_OVER, lit, signIn, t } from './harness.mjs';

export const area = {
  name: 'relay',
  needs: [],
  async run(h) {
    const { url, user, password, relayTLS } = h.opts;
    const address = new URL(url);
    if (relayTLS && address.protocol !== 'https:') throw new Error('--relay-tls needs an HTTPS --url');
    const tlsURL = relayTLS ? `turns:${address.hostname}:${address.port || 443}?transport=tcp` : null;
    const p = await h.contextPage();
    await p.send('Page.addScriptToEvaluateOnNewDocument', { source: `{
      const P = RTCPeerConnection, tlsURL = ${lit(tlsURL)};
      window.RTCPeerConnection = function (c) {
        const iceServers = tlsURL ? c.iceServers.filter((s) => s.urls.includes(tlsURL)).map((s) => ({ ...s, urls: [tlsURL] })) : c.iceServers;
        if (tlsURL && !iceServers.length) throw new Error('Server did not advertise ' + tlsURL);
        const pc = new P({ ...c, iceServers, iceTransportPolicy: 'relay' });
        if (tlsURL) {
          let offeredAt;
          window.relayCandidateTimes = [];
          const offer = pc.setLocalDescription.bind(pc), add = pc.addIceCandidate.bind(pc);
          pc.setLocalDescription = (d) => { offeredAt = performance.now(); return offer(d); };
          pc.addIceCandidate = (c) => { window.relayCandidateTimes.push(performance.now() - offeredAt); return add(c); };
        }
        return pc;
      };
      window.RTCPeerConnection.prototype = P.prototype;
    }` });
    await p.send('Page.navigate', { url });
    await p.until(`!!document.querySelector('form.login')`, 'login form');
    await signIn(p, { user, password });
    await p.until(`!!document.querySelector('.page[data-page="choose"]')`, 'choosing its passwords');
    const passwords = ['relay own password', 'relay own password', 'relay super password', 'relay super password'];
    await p.eval(`(() => { const ins = document.querySelectorAll('.choose input'); ${lit(passwords)}.forEach((v, i) => { ins[i].value = v; }); document.querySelector('.choose button[type="submit"]').click(); })()`);
    await p.until(`document.querySelector('.dialog h2')?.textContent.includes(${lit(t('approve.computerTitle'))})`, 'computer approval');
    await p.clickText('.dialog button', t('common.approve'));
    await p.until(`!!document.querySelector('.card')`, 'card');
    await p.until(`document.querySelector('.card .status')?.textContent.includes(${lit(t('status.online'))})`, 'computer online');
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
    if (relayTLS) {
      h.check('the browser used TURN over TLS on the HTTPS port', st.relayProtocol === 'tls');
      const times = await p.v('window.relayCandidateTimes');
      h.check('the host only found the TLS relay with the usual TURN port blocked', times.length === 1);
      const [first] = times;
      // A silent TURN server spends at least six 300 ms waits in its first exchange.
      h.check('the host offered TLS before the blocked TURN attempts timed out', first >= 0 && first < 1800, `${Math.round(first)} ms after offer`);
    }
    h.check('the host really used a relay', st.remote === 'relay');
    h.check('frames keep decoding', st.decoded > 0);
    h.check('the input channel opens over the relay', st.input === 'open');
  },
};
