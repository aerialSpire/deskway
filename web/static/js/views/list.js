// My computers: the account's computers, with their previews and vitals.
import { $app, h, loading } from '../dom.js';
import { buzz } from '../env.js';
import { t } from '../i18n.js';
import { state, setView, render } from '../state.js';
import { statusOf, statusLine, staleNote, thumbFor, topbar, heading, languagePicker, updateNote, hereTag } from './common.js';
import { accountMenu } from './account.js';
import { computerMenu } from './computer.js';
import { vitalsBars } from './vitals.js';

export function renderList() {
  const online = state.devices.filter((d) => d.online).length;
  const header = topbar(languagePicker(() => render()), accountMenu());
  const title = heading(t('nav.myComputers'), {
    beside: state.devices.length ? h('div', { class: 'count' }, t('list.count', { online, total: state.devices.length })) : null,
  });
  const stale = state.socketDown ? staleNote('list.stale') : null;
  const body = !state.devicesLoaded
    ? loading(state.socketDown ? t('list.connecting') : t('list.loading'))
    : state.devices.length
    ? h('div', { class: 'grid' }, state.devices.map(card))
    : h('div', { class: 'empty' }, h('h2', {}, t('list.emptyTitle')), h('p', {}, t('list.emptyText')));
  $app.replaceChildren(h('div', { class: 'page' }, header, updateNote(), title, stale, body));
}

// A computer: anywhere on its card opens it (its name is the button, stretched over the card);
// its ⋯ sits at the end of the name's row, and holding the card opens the same menu.
function card(d) {
  const menu = computerMenu(d);
  const el = h('div', { class: 'card' },
    h('div', { class: `thumb ${d.online ? '' : 'offline'}` }, thumbFor(d)),
    h('div', { class: 'card-body' },
      h('div', { class: 'card-head' },
        h('div', { class: 'card-name' },
          h('button', { class: 'card-open', 'aria-label': t('list.card', { name: d.name, status: statusOf(d).text }), onclick: () => setView('detail', d.id) }, d.name),
          hereTag(d)),
        menu.el),
      statusLine(d),
      vitalsBars(d)));
  holdToOpen(el, menu);
  return el;
}

// Holding the card (or right-clicking it) opens its menu; the press that does is not also a
// click that opens the computer. Presses on the ⋯ or in the menu are theirs.
function holdToOpen(el, menu) {
  let timer = null;
  let start = null;
  let held = false;
  const cancel = () => { clearTimeout(timer); timer = null; };
  // `hold`: opened by a press still down, whose click is to be dropped.
  const open = (hold) => { cancel(); held = hold; buzz(15); menu.open(); };
  const mine = (e) => !menu.el.contains(e.target);
  el.addEventListener('pointerdown', (e) => {
    held = false;
    if (e.button !== 0 || !mine(e)) return;
    start = [e.clientX, e.clientY];
    cancel();
    timer = setTimeout(() => open(true), 550);
    // Lifting anywhere ends the hold, even if this card was replaced meanwhile.
    const lift = () => { cancel(); document.removeEventListener('pointerup', lift, true); document.removeEventListener('pointercancel', lift, true); };
    document.addEventListener('pointerup', lift, true);
    document.addEventListener('pointercancel', lift, true);
  });
  el.addEventListener('click', (e) => { if (held && mine(e)) { held = false; e.preventDefault(); e.stopImmediatePropagation(); } }, true);
  el.addEventListener('pointermove', (e) => { if (timer && Math.hypot(e.clientX - start[0], e.clientY - start[1]) > 10) cancel(); });
  // A right click, or a touch hold the browser reports as one (its own menu never shows: app-feel.js):
  // the card's menu, unless the hold has opened it already.
  el.addEventListener('contextmenu', (e) => { if (mine(e) && !held) open(!!timer); });
}

// A thumbnail from the server: into the list's cards and the detail page's display tiles.
export function onThumb(m) {
  const url = `data:image/jpeg;base64,${m.data}`;
  if (m.screen) {
    state.screenThumbs[`${m.device}|${m.screen}`] = url;
    for (const img of document.querySelectorAll('img[data-screen-thumb]')) {
      if (img.dataset.screenThumb === `${m.device}|${m.screen}`) { img.src = url; img.hidden = false; img.parentElement?.querySelector('.display-wait')?.remove(); }
    }
  }
  // The list card shows the primary display (or the only one).
  const d = state.devices.find((x) => x.id === m.device);
  const primary = d && Array.isArray(d.displays) ? (d.displays.find((x) => x.primary) || d.displays[0]) : null;
  if (m.screen && primary && primary.id !== m.screen) return;
  state.thumbs[m.device] = url;
  for (const img of document.querySelectorAll(`img[data-thumb="${m.device}"]`)) {
    img.src = state.thumbs[m.device];
    img.hidden = false;
  }
  for (const p of document.querySelectorAll(`[data-placeholder="${m.device}"]`)) p.remove();
}
