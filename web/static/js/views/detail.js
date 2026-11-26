// A computer's page: its name with the way back and its ⋯, its status, and its picture (or its
// displays, where they sit), followed by the available actions and AI activity.
import { $app, h, loading } from '../dom.js';
import { agoText, t } from '../i18n.js';
import { state, device, setView } from '../state.js';
import { statusLine, staleNote, thumbFor, topbar, heading, updateNote, hereTag, noDesktop } from './common.js';
import { accountMenu } from './account.js';
import { computerMenu } from './computer.js';
import { press } from './takeover.js';
import { auditSection } from './audit.js';
import { chosenDisplay, rememberDisplay, orderedDisplays } from '../session/displays.js';
import { CAPS } from '../proto.js';
import { keptTerminalNotice } from '../terminal-ui.js';

let keptAge;
const terminalAge = (since) => agoText(since, { seen: false });

export function renderDetail() {
  clearTimeout(keptAge);
  const d = device();
  if (!d && !state.devicesLoaded) {
    $app.replaceChildren(h('div', { class: 'page' }, topbar(accountMenu()), heading('', { back: true }), loading(t('common.loading'))));
    return;
  }
  if (!d) { setView('list'); return; }
  // Pressing the picture takes over. With several displays, each is shown live where it sits,
  // and pressing one takes over that one.
  const preview = noDesktop(d)
    ? h('p', { class: 'notice' }, t('session.noDesktop'))
    : d.online
      ? displayLayout(d) || h('button', { class: 'preview clickable', 'aria-label': t('detail.takeOverLabel', { name: d.name }), onclick: () => press(device() || d) }, hint(), thumbFor(d))
      : h('div', { class: 'preview offline' }, h('div', { class: 'thumb-inner' }, t('detail.offline')));
  const kept = keptTerminalNotice(d, t, terminalAge);
  const keptText = kept ? h('span', { class: 'text' }, kept) : null;
  const openTerminal = () => press(device() || d, false, true);
  $app.replaceChildren(h('div', { class: 'page', 'data-page': 'detail' },
    topbar(accountMenu()),
    updateNote(),
    heading(d.name, { back: true, beside: hereTag(d), end: computerMenu(d).el, sub: statusLine(d) }),
    state.socketDown ? staleNote('detail.stale') : null,
    preview,
    kept ? h('div', { class: 'notice terminal-kept', role: 'status' },
      keptText,
      h('button', { class: 'btn compact', type: 'button', onclick: openTerminal }, t('terminal.return'))) : null,
    !kept && d.online && d.caps?.includes(CAPS.TERMINAL)
      ? h('div', { class: 'computer-actions' },
        h('button', { class: 'btn', type: 'button', onclick: openTerminal }, t('terminal.label')))
      : null,
    auditSection(d)));
  if (keptText) {
    function updateAge() {
      if (!keptText.isConnected) return;
      keptText.textContent = keptTerminalNotice(d, t, terminalAge);
      keptAge = setTimeout(updateAge, 60000);
    }
    keptAge = setTimeout(updateAge, 60000);
  }
}

// The one line over an online computer's picture: pressing a screen takes over.
const hint = () => h('div', { class: 'preview-hint' }, t('detail.hint'));

// The computer's displays, drawn where they sit relative to each other. Clicking one takes
// over on that display. Shown only when there is more than one.
function displayLayout(d) {
  const list = orderedDisplays(d.displays);
  if (list.length < 2) return null;
  const minX = Math.min(...list.map((x) => x.x));
  const minY = Math.min(...list.map((x) => x.y));
  const maxX = Math.max(...list.map((x) => x.x + x.w));
  const maxY = Math.max(...list.map((x) => x.y + x.h));
  const W = maxX - minX;
  const H = maxY - minY;
  const chosen = chosenDisplay(d.id);
  const tiles = list.map((x, i) => {
    const thumb = state.screenThumbs[`${d.id}|${x.id}`];
    const tile = h('button', {
      class: `display-tile ${x.id === chosen ? 'chosen' : ''}`,
      'aria-label': t(x.primary ? 'display.tileLabelMain' : 'display.tileLabel', { i: i + 1, size: `${x.w}×${x.h}` }),
      onclick: () => {
        rememberDisplay(d.id, x.id);
        press(device() || d);
      },
    },
    h('img', { alt: '', hidden: !thumb, src: thumb || undefined, 'data-screen-thumb': `${d.id}|${x.id}` }),
    thumb ? null : h('span', { class: 'display-wait' }, t('thumb.loading')),
    h('span', { class: 'display-label' }, x.primary ? t('display.tileMain', { i: i + 1 }) : String(i + 1)));
    Object.assign(tile.style, { left: `${((x.x - minX) / W) * 100}%`, top: `${((x.y - minY) / H) * 100}%`, width: `${(x.w / W) * 100}%`, height: `${(x.h / H) * 100}%` });
    return tile;
  });
  const map = h('div', { class: 'display-map' }, tiles);
  map.style.aspectRatio = `${W} / ${H}`;
  map.style.width = `min(100%, calc(var(--preview-h) * ${(W / H).toFixed(4)}))`;
  return h('section', { class: 'preview' },
    hint(),
    map);
}
