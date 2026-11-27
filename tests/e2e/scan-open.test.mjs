import { issueAdminLink, lit, redeemSetup, words } from './harness.mjs';

export const area = {
  name: 'scan-open',
  needs: [],
  async run(h) {
    const { url, serverExe, serverState } = h.opts;
    const lang = 'zh-Hans';
    const page = await h.newPage(lang);
    const setup = await issueAdminLink(serverExe, serverState, url);
    await redeemSetup(page, setup, { user: 'scan-open', password: 'scan-open test password' });
    const { local_mode: local, local_guide_url: guide } = await page.v('d.me()');
    const expectedURL = local ? guide : new URL('/', url).href;
    const mode = local ? 'local' : 'public';
    const title = words('scanOpen.title', {}, { lang });
    const scan = `.topbar button[aria-label=${lit(title)}]`;
    await page.eval(`document.querySelector(${lit(scan)}).click()`);
    const shown = await page.eval(`({
      url: document.querySelector('.qr-addr .copyable-code').textContent,
      lead: document.querySelector('.qr-lead').textContent,
      qr: !!document.querySelector('.qr svg')
    })`);
    h.check(`${mode} mode shows the address and QR code`, shown.qr && shown.url === expectedURL, shown.url);
    const lead = words(local ? 'scanOpen.localLead' : 'scanOpen.lead', {}, { lang });
    h.check(`${mode} mode shows the scanning instructions`, shown.lead === lead, shown.lead);
    if (local) await page.shot('scan-open-local', 1280, 800);
  },
};
