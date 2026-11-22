// Languages: English, Simplified Chinese, Traditional Chinese (docs/i18n.md). The first visit's
// choice from the browser's languages, the switchers, and every main screen in each language:
// no keys shown, no words in the other script. Screenshots of each.
import { sleep, LANGS, WEB_TEXTS, clipboardStub, transferCard, endTransferCard } from './harness.mjs';

const CJK = /[⺀-⿿　-ヿ㐀-䶿一-鿿豈-﫿︰-﹏＀-￯]/g;
// Characters written differently in the two scripts: one script's never belong on the other's page.
const SIMPLIFIED_ONLY = /[们这个电脑贴设备账号码连断键标传输图发远录开关时线务网络载机动应对级认证为览复盘选择删页锁写读许请验钥无让种两过还没]/g;
const TRADITIONAL_ONLY = /[們這個電腦貼設備帳號碼連斷鍵標傳輸圖發遠錄開關時線務網絡載機動應對級認證為覽複盤選擇刪頁鎖寫讀許請驗鑰無讓種兩過還沒]/g;
// Words the page shows that the person or the computer chose (names), and the language
// switcher, which lists each language in its own name: not the page's own text.
// My devices' rows as laid out now: what is wrong with them, if anything.
const ROWS = `(() => {
  const bad = [];
  const page = document.scrollingElement.scrollWidth <= innerWidth + 1;
  if (!page) bad.push('the page scrolls sideways');
  for (const row of document.querySelectorAll('.device-row')) {
    const r = row.getBoundingClientRect();
    const name = row.querySelector('.device-name .name');
    const text = row.querySelector('.device-text');
    const line = parseFloat(getComputedStyle(name).lineHeight) || 24;
    if (name.getBoundingClientRect().height > line * 1.5) bad.push('name on several lines: ' + name.textContent);
    if (name.scrollWidth > name.clientWidth + 1 && getComputedStyle(name).textOverflow !== 'ellipsis') bad.push('name cut without …');
    if (text.getBoundingClientRect().width < 120) bad.push('name squeezed to ' + Math.round(text.getBoundingClientRect().width) + 'px');
    for (const e of row.querySelectorAll('.device-sub .tag, .device-actions .btn')) {
      const b = e.getBoundingClientRect();
      if (b.width && (b.right > r.right + 0.5 || b.left < r.left - 0.5)) bad.push('out of its row: ' + (e.textContent || e.className).trim().slice(0, 30));
    }
  }
  // A computer's AI settings, in its card: the words beside the switch, the switch whole and inside.
  for (const row of document.querySelectorAll('.device-settings .setting-row')) {
    const r = row.getBoundingClientRect(), sw = row.querySelector('.switch').getBoundingClientRect();
    if (sw.right > r.right + 0.5 || sw.width < 30) bad.push('a switch squeezed or out of its row');
  }
  return { ok: !bad.length, bad };
})()`;

// The AI activity as laid out now: what is wrong with it, if anything.
const AUDIT = `(() => {
  const bad = [];
  if (document.scrollingElement.scrollWidth > innerWidth + 1) bad.push('the page scrolls sideways');
  const box = document.querySelector('.audit').getBoundingClientRect();
  for (const e of document.querySelectorAll('.audit-filters > *, .audit-pager > *, .audit h2')) {
    const b = e.getBoundingClientRect();
    if (b.right > box.right + 0.5 || b.left < box.left - 0.5) bad.push('out of the section: ' + (e.textContent || e.getAttribute('aria-label') || e.tagName).trim().slice(0, 30));
  }
  const heights = [...document.querySelectorAll('.audit-pager .btn')].map((b) => Math.round(b.getBoundingClientRect().height));
  if (new Set(heights).size > 1) bad.push('pager buttons of different heights');
  const results = [...document.querySelectorAll('.audit .audit-call .col-result')].map((c) => c.textContent);
  if (results.some((t) => /^[a-z_]+$/.test(t))) bad.push('a result shown as a code: ' + results.join(', '));
  // One scroll, the page's: nothing in the section is wider than it.
  for (const e of document.querySelectorAll('.audit-list, .audit-call, .audit-line, .audit-detail')) if (e.scrollWidth > e.clientWidth + 1) bad.push('wider than its place: ' + e.className);
  for (const line of document.querySelectorAll('.audit .audit-call .audit-line')) {
    const b = line.getBoundingClientRect();
    if (b.right > box.right + 0.5 || b.left < box.left - 0.5) bad.push('a call out of the section');
  }
  return { ok: !bad.length, bad };
})()`;

// A device's ⋯ is named after the device ("Manage “Windows PC · Chrome”"); its words are
// checked on the computers' cards, where the name is the test computer's.
const DATA = '.lang-btn, .menu-item[lang], .device-name > .name, .device-actions .more, .card-open, .tool-name-text, .menu-head, .name-btn, [data-page="detail"] .heading h1';
const pageWords = `(() => {
  const skip = ${JSON.stringify(DATA)};
  const parts = [];
  const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let n; (n = walk.nextNode());) {
    const e = n.parentElement;
    if (!e || e.closest(skip) || e.closest('script, style, textarea')) continue;
    if (!e.getClientRects().length || getComputedStyle(e).visibility === 'hidden') continue;
    if (n.textContent.trim()) parts.push(n.textContent.trim());
  }
  for (const e of document.querySelectorAll('[aria-label], [title], [placeholder], [data-drop], [data-hint]')) {
    if (e.closest(skip)) continue;
    for (const a of ['aria-label', 'title', 'placeholder', 'data-drop', 'data-hint']) if (e.getAttribute(a)) parts.push(e.getAttribute(a));
  }
  return parts.join('\\n');
})()`;

// Picks a language from the top bar's language menu, as a person would: open it, choose.
const pickLang = (p, lang, scope = '') => p.eval(`(() => {
  const b = document.querySelector(${JSON.stringify(`${scope} .lang-btn`.trim())});
  b.click();
  b.parentElement.querySelector('.menu-item[lang=${JSON.stringify(lang)}]').click();
})()`);

export const area = {
  name: 'i18n',
  needs: ['auth'],
  async run(h) {
    const cats = WEB_TEXTS;
    const tr = (l, key) => cats[l][key];
    const catalogKeys = Object.keys(cats.en);
    // What is wrong with the words on the page for `lang`: a key shown instead of its text, or
    // the wrong script. `expect`: words that must be there.
    const wordProblems = async (p, lang, expect = []) => {
      const text = await p.eval(pageWords);
      const out = [];
      const raw = text.match(/\b[a-z]+\.[a-z_.]+\b/g);
      if (raw) out.push(`raw keys: ${[...new Set(raw)].join(' ')}`);
      const keysShown = catalogKeys.filter((k) => text.includes(k));
      if (keysShown.length) out.push(`keys: ${keysShown.join(' ')}`);
      const cjk = text.match(CJK);
      if (lang === 'en' && cjk) out.push(`Chinese on an English page: ${[...new Set(cjk)].join('')}`);
      if (lang !== 'en' && !cjk) out.push('no Chinese at all');
      const wrong = text.match(lang === 'zh-Hant' ? SIMPLIFIED_ONLY : lang === 'zh-Hans' ? TRADITIONAL_ONLY : null);
      if (wrong) out.push(`other script: ${[...new Set(wrong)].join('')}`);
      for (const w of expect) if (!text.includes(w)) out.push(`missing ${JSON.stringify(w)}`);
      return out;
    };
    const checkWords = async (p, lang, where, expect) => {
      const problems = await wordProblems(p, lang, expect);
      h.check(`${lang}: ${where} shows its own words only, in the right script`, !problems.length, problems.join('; ').slice(0, 400));
    };

    // The switcher of a top-level page: in the top bar, at its right end.
    const TOP_RIGHT = `(() => { const s = document.querySelector('.lang-btn'), bar = s && s.closest('.topbar'); if (!bar) return false; const r = s.getBoundingClientRect(), b = bar.getBoundingClientRect(), page = bar.parentElement.getBoundingClientRect(); return r.top >= b.top && r.bottom <= b.bottom && r.left > b.left + b.width / 2 && b.right <= page.right + 1; })()`;

    // A browser seen for the first time picks from its languages; nothing is stored yet.
    const freshPage = async (languages) => {
      const p = await h.contextPage({ lang: null });
      await p.send('Emulation.setLocaleOverride', { locale: languages[0].replace(/-/g, '_') });
      await p.send('Page.addScriptToEvaluateOnNewDocument', { source: `Object.defineProperty(Navigator.prototype, 'languages', { get: () => Object.freeze(${JSON.stringify(languages)}) }); Object.defineProperty(Navigator.prototype, 'language', { get: () => ${JSON.stringify(languages[0])} });` });
      await p.send('Page.navigate', { url: h.opts.url });
      await p.until(`!!document.querySelector('form.login') && !!document.querySelector('.heading h1')`, `sign-in page for ${languages}`);
      return p;
    };
    for (const [languages, want] of [[['en-US'], 'en'], [['fr-FR'], 'en'], [['zh-TW'], 'zh-Hant'], [['zh-HK'], 'zh-Hant'], [['zh-MO'], 'zh-Hant'], [['zh-Hant-TW'], 'zh-Hant'], [['zh-CN'], 'zh-Hans'], [['zh', 'en-US'], 'zh-Hans'], [['fr-FR', 'zh-TW'], 'en']]) {
      const p = await freshPage(languages);
      const got = await p.eval(`({ lang: document.documentElement.lang, title: document.querySelector('.heading h1').textContent, picked: document.querySelector('.lang-btn').dataset.lang, stored: localStorage.getItem('deskway.lang') })`);
      h.check(`first visit with languages ${languages.join(', ')}: the sign-in page is in ${want}, and nothing is stored yet`, got.lang === want && got.title === tr(want, 'login.title') && got.picked === want && got.stored === null, JSON.stringify(got));
      await h.dropContext(p);
    }
    {
      const p = await freshPage(['en-US']);
      await p.eval(`document.querySelector('.login input').value = 'someone'`);
      const before = await p.eval(`performance.timeOrigin`);
      await pickLang(p, 'zh-Hant');
      await p.until(`document.querySelector('.heading h1')?.textContent === ${JSON.stringify(tr('zh-Hant', 'login.title'))}`, 'sign-in page switched to zh-Hant', 3000);
      h.check('the switcher on the sign-in page changes the language at once, without reloading, keeping what was typed', await p.eval(`document.documentElement.lang === 'zh-Hant' && localStorage.getItem('deskway.lang') === 'zh-Hant' && document.querySelector('.login input').value === 'someone'`) && await p.eval(`performance.timeOrigin`) === before);
      await p.eval(`location.reload()`);
      await sleep(500);
      await p.until(`!!document.querySelector('form.login') && !!document.querySelector('.heading h1')`, 'sign-in page after reload');
      h.check('the chosen language stays after a reload', await p.eval(`document.documentElement.lang === 'zh-Hant' && document.querySelector('.heading h1').textContent === ${JSON.stringify(tr('zh-Hant', 'login.title'))} && document.querySelector('.lang-btn').dataset.lang === 'zh-Hant'`));
      for (const lang of LANGS) {
        await pickLang(p, lang);
        await p.until(`document.documentElement.lang === ${JSON.stringify(lang)}`, `sign-in page in ${lang}`, 3000);
        const expect = ['login.title', 'login.user', 'login.password'].map((k) => tr(lang, k));
        await checkWords(p, lang, 'the sign-in page', expect);
        h.check(`${lang}: the switcher sits at the top right of the sign-in page, in the top bar`, await p.eval(TOP_RIGHT));
        await p.shot(`i18n-${lang}-login-wide`, 1440, 900);
        await p.shot(`i18n-${lang}-login-phone`, 390, 844);
      }
      await h.dropContext(p);
    }

    // Signed in, in each language: the computer list, my devices, a computer, and a session.
    await h.freeComputer();
    for (const lang of ['en', 'zh-Hant', 'zh-Hans']) {
      const L = await h.newPage(lang);
      await L.until(`!!document.querySelector('.card')`, `${lang}: computer list`);
      h.check(`${lang}: the page says its language (<html lang="${lang}">)`, await L.eval(`document.documentElement.lang`) === lang);
      await checkWords(L, lang, 'the computer list', [tr(lang, 'nav.myComputers')]);
      await L.shot(`i18n-${lang}-list-wide`, 1440, 900);
      await L.shot(`i18n-${lang}-list-phone`, 390, 844);
      await L.metrics(1440, 900);

      h.check(`${lang}: on my computers the switcher sits at the top right, in the top bar, beside the account`, await L.eval(TOP_RIGHT) && await L.eval(`!!document.querySelector('.topbar .name-btn') && document.querySelector('.lang-btn').dataset.lang === ${JSON.stringify(lang)}`));
      const listed = JSON.stringify(LANGS.map((l) => `${l}:${tr(lang, `lang.${l}`)}`));
      h.check(`${lang}: the language menu is the page's own menu: every language in its own words, the current one checked`, await L.eval(`(() => {
        const b = document.querySelector('.lang-btn');
        b.click();
        const items = [...b.parentElement.querySelectorAll('.menu-item')];
        const ok = JSON.stringify(items.map((i) => i.lang + ':' + i.textContent)) === ${JSON.stringify(listed)}
          && items.filter((i) => i.getAttribute('aria-checked') === 'true').map((i) => i.lang).join() === ${JSON.stringify(lang)}
          && !document.querySelector('select');
        b.click();
        return ok;
      })()`));
      if (lang === 'en') {
        // Switched on my computers: shown at once in the new language, and back.
        const before = await L.eval(`performance.timeOrigin`);
        await pickLang(L, 'zh-Hant', '.topbar');
        await L.until(`document.querySelector('.heading h1')?.textContent === ${JSON.stringify(tr('zh-Hant', 'nav.myComputers'))}`, 'my computers switched to zh-Hant', 3000);
        h.check('the switcher on my computers changes the language at once, without reloading', await L.eval(`document.documentElement.lang === 'zh-Hant' && document.querySelectorAll('.card').length >= 1`) && await L.eval(`performance.timeOrigin`) === before);
        await checkWords(L, 'zh-Hant', 'my computers, just switched', [tr('zh-Hant', 'nav.myComputers')]);
        await pickLang(L, 'en', '.topbar');
        await L.until(`document.querySelector('.heading h1')?.textContent === ${JSON.stringify(tr('en', 'nav.myComputers'))}`, 'my computers back in English', 3000);
      }

      await L.v(`d.show('devices')`);
      await L.until(`document.querySelectorAll('.device-row').length >= 2`, `${lang}: my devices`);
      // The hardest row: a computer with a very long name, AI and administrator rights on (both
      // switches showing), at every width: the name on one line ended with …, nothing out of
      // its row, no sideways scrolling.
      await L.v(`d.serverSays({ type: 'devices', devices: d.devices().map((x) => ({ ...x, name: 'Upstairs study room computer with a very long name', ai: { state: 'on', admin: true, ts: 1 } })) })`);
      await L.until(`!!document.querySelector('[data-ai-admin]')`, `${lang}: both AI switches`);
      for (const [w, hgt] of [[1440, 900], [768, 1024], [390, 844], [320, 640]]) {
        await L.metrics(w, hgt, false, 1);
        await sleep(100);
        const l = await L.eval(ROWS);
        h.check(`${lang} ${w}px: my devices: names on one line ended with …, ⋯ and switches inside their rows, nothing scrolls sideways`, l.ok, JSON.stringify(l));
        if (w === 390 || w === 320) await L.shot(`i18n-${lang}-devices-ai-${w}`, w, hgt);
      }
      await L.metrics(1440, 900);
      await checkWords(L, lang, 'my devices', [tr(lang, 'devices.title')]);
      h.check(`${lang}: a sub-page (my devices) has the same top bar, without the switcher`, await L.eval(`!!document.querySelector('.topbar .brand') && !document.querySelector('.lang-btn')`));
      await L.shot(`i18n-${lang}-devices-wide`, 1440, 900);
      await L.metrics(390, 844, false, 1);
      await L.eval(`scrollTo(0, document.documentElement.scrollHeight)`);
      await L.shot(`i18n-${lang}-devices-phone-end`, 390, 844);
      await L.eval(`scrollTo(0, 0)`);
      await L.metrics(1440, 900);

      await L.v(`d.show('detail', d.devices()[0].id)`);
      await L.until(`document.querySelectorAll('.display-tile').length === 2`, `${lang}: a computer`);
      await checkWords(L, lang, 'a computer', [tr(lang, 'detail.hint')]);
      await L.shot(`i18n-${lang}-detail-wide`, 1440, 900);
      // The AI activity below the screens, opened, with a page of calls as a computer answers one.
      await L.eval(`document.querySelector('.audit-toggle').click()`);
      const call = (i, result) => ({ at: 1795000000 - i * 60, client: 'Claude Code', device: 'c', session: 's', tool: ['run', 'screenshot', 'fs_write'][i % 3], args: '{"command":"npm test -- --watch=false --reporter=verbose"}', result, detail: '', ms: 1200 });
      const calls = JSON.stringify([call(0, 'ok'), call(1, 'running'), call(2, 'elevation_unavailable')]);
      await L.v(`d.serverSays({ type: 'audit', device: d.devices()[0].id, page: { calls: ${calls}, total: 180, clients: ['Claude Code', 'Cursor'], tools: ['fs_write', 'run', 'screenshot'] } })`);
      await L.until(`document.querySelectorAll('.audit .audit-call').length === 3`, `${lang}: the AI activity`);
      // The failed one open: its details in place.
      await L.eval(`document.querySelectorAll('.audit .audit-call .audit-line')[2].click()`);
      await L.until(`!!document.querySelector('.audit-call.open .audit-detail')`, `${lang}: a call opened`);
      for (const [w, hgt] of [[1440, 900], [768, 1024], [390, 844], [320, 640]]) {
        await L.metrics(w, hgt, false, 1);
        await sleep(100);
        await L.eval(`document.querySelector('.audit').scrollIntoView()`);
        const l = await L.eval(AUDIT);
        h.check(`${lang} ${w}px: the AI activity: filters, calls, details and pages inside it, results in words, nothing scrolls sideways`, l.ok, JSON.stringify(l));
        await L.shot(`i18n-${lang}-audit-${w}`, w, hgt);
      }
      await L.eval(`scrollTo(0, 0)`);
      await L.metrics(1440, 900);

      // A session. This computer's clipboard is stubbed; nothing of it is read.
      await L.until(`d.devices()[0] && !d.devices()[0].controller`, `${lang}: the computer is free`, 30000);
      await clipboardStub(L, { deny: true });
      await L.v(`d.open(d.devices()[0].id, { force: true })`);
      await L.until(`!!s && s.info().connection === 'connected' && s.info().w > 0 && s.el.routeDot.className === 'dot ok'`, `${lang}: session`);
      await L.v(`(s.fromHost({ t: 'clipshare', on: true }), s.fromHost({ t: 'cb', gen: 1, owner: 'host', formats: [{ kind: 'files', items: [{ n: 'Q3 Report.PDF', s: 40000 }, { n: 'Notes.TXT', s: 10 }] }] }))`);
      await L.v(`(s.showToolbar(true), s.togglePanel('clip'))`);
      await sleep(350);
      await checkWords(L, lang, 'a session: toolbar, notes and the clipboard panel', [tr(lang, 'clip.name'), tr(lang, 'clip.take'), tr(lang, 'clip.sendFiles')]);
      await L.shot(`i18n-${lang}-clipboard-wide`, 1440, 900);
      await L.shot(`i18n-${lang}-clipboard-phone`, 390, 844);
      await L.metrics(1440, 900);
      await L.v(`s.togglePanel('clip')`);
      await L.v(`(s.keyboard(true), document.querySelector('.kb-tip').hidden = false, s.el.padBtn && s.mousePad(true))`);
      await checkWords(L, lang, 'a session: keyboard and mouse panel', [tr(lang, 'kb.ime'), tr(lang, 'kb.imeTip'), tr(lang, 'kb.key.Space')]);
      await L.shot(`i18n-${lang}-keyboard-wide`, 1440, 900);
      await L.shot(`i18n-${lang}-keyboard-phone`, 390, 844);
      await L.metrics(1440, 900);
      await L.v(`(document.querySelector('.kb-tip').hidden = true, s.keyboard(false), s.el.mousePad && s.mousePad(false))`);
      // A transfer card: two files under way with one more waiting, on a channel that never
      // answers; then one refused for its size.
      await transferCard(h, L, lang, '(1440 wide)');
      await checkWords(L, lang, 'a session: a transfer under way', [tr(lang, 'xfer.cancel')]);
      await L.shot(`i18n-${lang}-transfer-wide`, 1440, 900);
      await L.metrics(1440, 900);
      await endTransferCard(L);
      await L.v(`s.sendFiles([new File([new Uint8Array(26 << 20)], 'Big.BIN')])`);
      await checkWords(L, lang, 'a session: a refused transfer', [tr(lang, 'xfer.tooBig')]);
      await L.v(`s.dismissTransfer()`);

      await L.v(`s.leave()`);
      await L.until(`d.view() === 'detail' && !s`, `${lang}: left the session`);
      await h.closePage(L);
    }
    // The pages above share this browser's storage: back to the language the other checks read.
    await h.a.eval(`localStorage.setItem('deskway.lang', 'en')`);
  },
};
