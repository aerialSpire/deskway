// Layout of the session at a phone's, a tablet's and a wide width (390, 768, 1440), in English,
// the longest words: the toolbar and its panels, then, with something copied waiting, also the
// clipboard panel, the keyboard bar, the ball menu, the transfer card and the disconnect question.
import { sleep, toolbarLayout, toolbarFits, transferCard, endTransferCard, t, q } from './harness.mjs';

const SIZES = [[320, 640, true], [390, 844, true], [768, 1024, true], [900, 700, false], [1024, 768, false], [1440, 900, false]];
// An AI client controlling the computer too: the bar says who along its bottom.
const AI_NAME = 'Claude Code with a long device name';
const AI_ON = `d.serverSays({ type: 'devices', devices: d.devices().map((x) => ({ ...x, ai: { state: 'on', admin: false, ts: 1, name: '${AI_NAME}' } })) })`;
const AI_LINE = `(() => { const a = document.querySelector('.toolbar .tool-ai'), x = a.querySelector('.text'); return !a.hidden && x.title.includes('${AI_NAME}') && getComputedStyle(x).whiteSpace === 'nowrap'; })()`;

export const area = {
  name: 'layout',
  needs: ['auth'],
  async run(h) {
    const b = await h.secondPage();
    await h.freeComputer(b);
    await h.sessionOn(b);
    await b.v(AI_ON);
    await b.metrics(1440, 900);
    h.check('toolbar: every control has the same height', await b.eval(`(() => { const hs = [...document.querySelectorAll('.toolbar .tool')].filter((e) => e.offsetParent).map((e) => e.getBoundingClientRect().height); return hs.length > 3 && hs.every((x) => x === hs[0]); })()`));
    h.check('toolbar: one way out, no separate back button', await b.eval(`!document.querySelector('.toolbar .tool.icon') && [...document.querySelectorAll('.toolbar button')].filter((x) => x.textContent === ${q('tb.disconnect')}).length === 1`));
    // An AI client working there: said once over the picture, and on the tab while the bar is away.
    h.check('an AI client starting to control it is said over the picture', await b.eval(`[...document.querySelectorAll('.notice')].some((n) => n.textContent.includes('${AI_NAME}'))`));
    // The toolbar floats and stays out of the way; shown, it fits any window and nothing overlaps.
    await b.v(`s.hideToolbar()`);
    await sleep(350);
    h.check('the tab in sight while the bar is away says which AI client controls it', await b.eval(`(() => { const t = document.querySelector('.toolbar-tab'), m = t.querySelector('.tab-ai'), r = t.getBoundingClientRect(); return t.classList.contains('ai') && !m.hidden && m.title.includes('${AI_NAME}') && r.top >= 0 && r.bottom > 0 && r.left >= 0 && r.right <= innerWidth; })()`));
    for (const [w, hgt, mobile] of [[390, 844, true], [1440, 900, false]]) {
      await b.metrics(w, hgt, mobile);
      await b.shot(`session-ai-tab-${w}`, w, hgt);
    }
    await b.metrics(1440, 900);
    await sleep(350);
    h.check('toolbar: hidden above the screen until wanted, not drawn at all (no strip under a camera cutout), and the screen fills the window', await b.v(`(() => { const t = s.el.toolbar.getBoundingClientRect(), st = s.el.stage.getBoundingClientRect(); return t.bottom <= 0 && getComputedStyle(s.el.toolbar).visibility === 'hidden' && st.top <= 1 && Math.abs(st.height - innerHeight) <= 1; })()`), await b.v(`JSON.stringify({ bar: s.el.toolbar.getBoundingClientRect(), stage: s.el.stage.getBoundingClientRect(), h: innerHeight, w: innerWidth })`));
    for (const [w, hgt, mobile] of SIZES) {
      await b.metrics(w, hgt, mobile);
      await b.v(`s.showToolbar(true)`);
      await sleep(350);
      const layout = await b.v(toolbarLayout);
      const narrow = w <= 860;
      h.check(`toolbar at ${w}×${hgt}: inside the window, name and route readable, every button's words on one line, nothing overlapping or cut off, ${narrow ? 'the buttons in a second row' : 'one row'}, who else controls it in a line under it`, toolbarFits(layout, narrow), JSON.stringify(layout));
      h.check(`toolbar at ${w}×${hgt}: the AI client named in its line, on one line`, await b.eval(AI_LINE));
      await b.shot(`toolbar-${w}x${hgt}`, w, hgt);
      await b.v(`s.togglePanel('clip')`);
      h.check(`toolbar at ${w}×${hgt}: an open panel starts below the whole toolbar`, await b.v(`s.el.panels.getBoundingClientRect().top >= s.el.toolbar.getBoundingClientRect().bottom`), await b.v(`JSON.stringify([s.el.panels.getBoundingClientRect().top, s.el.toolbar.getBoundingClientRect().bottom])`));
      await b.v(`s.togglePanel('clip')`);
    }
    await b.v(`s.hideToolbar()`);
    await b.metrics(1440, 900);
    await b.v(`s.leave()`);

    // English is the longest: everything must still fit.
    const L = await h.newPage('en');
    await L.until(`!!document.querySelector('.card')`, 'en: computer list');
    await h.freeComputer(L);
    await h.sessionOn(L);
    await L.v(AI_ON);
    await L.v(`(s.fromHost({ t: 'clipshare', on: true }), s.fromHost({ t: 'cb', gen: 1, owner: 'host', formats: [{ kind: 'files', items: [{ n: 'Q3 Report.PDF', s: 40000 }, { n: 'Notes.TXT', s: 10 }] }] }))`);
    for (const [w, hgt, mobile] of SIZES) {
      await L.metrics(w, hgt, mobile);
      await L.v(`s.showToolbar(true)`);
      await sleep(350);
      const layout = await L.v(toolbarLayout);
      h.check(`en, toolbar at ${w}×${hgt}: inside the window, every button's words on one line, nothing overlapping or cut off, ${w <= 860 ? 'the buttons in a second row, sliding when they do not fit' : 'one row'}`, toolbarFits(layout, w <= 860), JSON.stringify(layout));
      h.check(`en, toolbar at ${w}×${hgt}: the AI client named in its line, on one line`, await L.eval(AI_LINE));
      await L.shot(`i18n-en-toolbar-${w}`, w, hgt);
      await L.metrics(w, hgt, mobile);
      await L.v(`s.togglePanel('clip')`);
      const clip = await L.eval(`(() => {
        const p = document.querySelector('.panel.clip').getBoundingClientRect();
        const bad = [...document.querySelectorAll('.panel.clip button')].filter((b) => { const r = b.getBoundingClientRect(); return b.scrollWidth > b.clientWidth + 1 || r.left < p.left - 0.5 || r.right > p.right + 0.5; }).map((b) => b.textContent || b.getAttribute('aria-label'));
        return { inside: p.left >= 0 && p.right <= innerWidth, bad };
      })()`);
      h.check(`en, clipboard panel at ${w}: inside the window, no button cut off`, clip.inside && !clip.bad.length, JSON.stringify(clip));
      await L.v(`s.togglePanel('clip')`);
      await L.v(`(s.keyboard(true), document.querySelector('.kb-tip').hidden = false)`);
      const kb = await L.eval(`(() => {
        const bar = document.querySelector('.kb-bar'), ime = bar.querySelector('.kb-ime'), tip = bar.querySelector('.kb-tip');
        const parts = [bar.querySelector('.kb-grip'), bar.querySelector('.kb-ime-wrap'), bar.querySelector('.kb-hide')].map((e) => e.getBoundingClientRect());
        const overlap = parts.some((a, i) => parts.some((b, j) => j > i && a.left < b.right - 0.5 && b.left < a.right - 0.5));
        const t = tip.getBoundingClientRect(), b = bar.getBoundingClientRect();
        return { ime: ime.scrollWidth <= ime.clientWidth + 1, overlap, tip: t.left >= 0 && t.right <= innerWidth && t.top >= 0, inBar: parts.every((r) => r.left >= b.left - 0.5 && r.right <= b.right + 0.5) };
      })()`);
      h.check(`en, keyboard bar at ${w}: "Input method", its tip and Hide keyboard fit, nothing overlapping`, kb.ime && !kb.overlap && kb.tip && kb.inBar, JSON.stringify(kb));
      await L.shot(`i18n-en-keyboard-${w}`, w, hgt);
      await L.metrics(w, hgt, mobile);
      await L.v(`(document.querySelector('.kb-tip').hidden = true, s.keyboard(false))`);
      await transferCard(h, L, 'en', `(${w} wide)`);
      await endTransferCard(L);
      await L.v(`s.confirmLeave()`);
      const dlg = await L.eval(`(() => { const d = document.querySelector('.dialog').getBoundingClientRect(); return [...document.querySelectorAll('.dialog button')].every((b) => { const r = b.getBoundingClientRect(); return r.left >= d.left && r.right <= d.right && b.scrollWidth <= b.clientWidth + 1; }) && d.left >= 0 && d.right <= innerWidth; })()`);
      h.check(`en, the disconnect question at ${w}: inside the window, buttons fit`, dlg);
      await L.clickText('.dialog button', t('common.cancel'));
    }
    await L.v(`s.hideToolbar()`);
    await L.metrics(1440, 900);
    await L.v(`s.leave()`);
    await L.until(`d.view() === 'detail' && !s`, 'en: left the session');
    await h.closePage(L);
    // The English page shares this browser's storage: back to the language the other checks read.
    await h.a.eval(`localStorage.setItem('deskway.lang', 'en')`);
  },
};
