// Use the auth fixture with the previous-version host built from
// ce14c938a6860f9f47d83bdf3aab13b371c1c4b2.
import { MIN_VERSION } from '../../web/static/js/proto.js';
import { lit, t } from './harness.mjs';

export const area = {
  name: 'previous-protocol',
  needs: ['auth'],
  async run(h) {
    const a = h.a;
    await a.until(`d.devices()[0]?.online && d.devices()[0].version === ${MIN_VERSION}`, 'previous-version host online');
    await a.v(`d.show('detail', d.devices()[0].id)`);
    h.check('the previous-version host has no terminal entry',
      await a.eval(`!!document.querySelector('[data-page="detail"]') && ![...document.querySelectorAll('[data-page="detail"] button')].some((b) => b.textContent === ${lit(t('terminal.label'))})`));
    await h.sessionOn(a);
    await a.until(`(await s.stats()).decoded > 0`, 'first picture from the previous-version host');
    h.check('a session with the previous-version host shows its first picture', true);
    await h.freeComputer(a);
  },
};
