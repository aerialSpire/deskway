// Use the auth fixture with a protocol 4 host, such as the build at 7b30e96.
import { MIN_VERSION } from '../../web/static/js/proto.js';
import { lit, t } from './harness.mjs';

export const area = {
  name: 'previous-protocol',
  needs: ['auth'],
  async run(h) {
    const a = h.a;
    await a.until(`d.devices()[0]?.online && d.devices()[0].version === ${MIN_VERSION}`, 'previous-version host online');
    await a.v(`d.show('detail', d.devices()[0].id)`);
    h.check('the previous-version host shows the update hint',
      await a.eval(`document.querySelector('[data-page="detail"] .computer-update')?.textContent === ${lit(t('update.available'))}`));
    await h.sessionOn(a);
    await a.until(`(await s.stats()).decoded > 0`, 'first picture from the previous-version host');
    h.check('a session with the previous-version host shows its first picture', true);
    await h.freeComputer(a);
  },
};
