// Impostors: a computer whose key differs from the one pinned in this browser, then an answer
// with a forged signature. Both are refused; with the real key it connects again.
import { sleep, SCREENS, TAKE_OVER, t, q } from './harness.mjs';

export const area = {
  name: 'security',
  needs: ['auth'],
  async run(h) {
    const p = h.a;
    await h.freeComputer(p);
    await p.send('Page.bringToFront');
    // The computer needs a moment to let go of the session just left.
    await sleep(2000);
    await p.metrics(1440, 900);
    await p.eval(`document.querySelector('.dialog button')?.click()`);
    await p.v(`d.show('detail', d.devices()[0].id)`);
    const pinName = `'deskway.hostkey.' + d.deviceId()`;
    const realPin = await p.v(`localStorage.getItem(${pinName})`);
    await p.v(`(async () => {
      const k = await crypto.subtle.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, true, ['sign', 'verify']);
      localStorage.setItem(${pinName}, btoa(String.fromCharCode(...new Uint8Array(await crypto.subtle.exportKey('spki', k.publicKey)))));
    })()`);
    await p.until(SCREENS, 'detail page before impostor test');
    await p.eval(TAKE_OVER);
    await p.until(`document.querySelector('.dialog h2')?.textContent.includes(${q('session.impostorTitle')})`, 'changed host key warning');
    h.check('a host whose key differs from the pinned one is refused', await p.eval(`document.querySelector('.dialog').textContent.includes(${q('session.keyChanged')}) && !document.querySelector('.session')`));
    await p.shot('impostor-warning', 1440, 900);
    await p.metrics(1440, 900);
    await p.clickText('.dialog button', t('common.gotIt'));
    // Back to the key pinned before (none, if this device had not met the computer yet).
    await p.v(realPin === null ? `localStorage.removeItem(${pinName})` : `localStorage.setItem(${pinName}, ${JSON.stringify(realPin)})`);
    // The answer's signature with one bit flipped, on its way from the server.
    await p.v(`d.interceptSignals((m) => {
      if (m.type !== 'answer' || !m.auth || !m.auth.sig) return m;
      window.__forged = (window.__forged || 0) + 1;
      const sig = Uint8Array.from(atob(m.auth.sig.replace(/-/g, '+').replace(/_/g, '/')), (c) => c.charCodeAt(0));
      sig[7] ^= 1;
      return { ...m, auth: { ...m.auth, sig: btoa(String.fromCharCode(...sig)) } };
    })`);
    await p.eval(TAKE_OVER);
    await p.until(`document.querySelector('.dialog h2')?.textContent.includes(${q('session.impostorTitle')})`, 'bad signature warning').catch(async (e) => {
      console.log(`      answers forged: ${await p.eval('window.__forged')}, stats: ${JSON.stringify(await p.v('s && s.stats()'))}`);
      throw e;
    });
    h.check('an answer with a forged signature is refused', await p.eval(`document.querySelector('.dialog').textContent.includes(${q('session.forged')}) && !document.querySelector('.session')`));
    await p.clickText('.dialog button', t('common.gotIt'));
    await p.v(`d.interceptSignals(null)`);
    await p.eval(TAKE_OVER);
    await p.until(`!!s && s.info().connection === 'connected'`, 'connects again with the real key');
    h.check('with the real key restored it connects again', true);
    await p.v(`s.leave()`);
  },
};
