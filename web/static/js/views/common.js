// Pieces every page shows (docs/design.md, Page structure): the top bar, the title, the way back,
// a notice line, a computer's status and thumbnail, the language switcher, a setting's row, the
// six digits two devices compare.
import { h, svg, spinner, ICONS } from '../dom.js';
import { dropdown } from './menu.js';
import { gotIt } from '../dialogs.js';
import { LANGS, currentLang, setLanguage, t, agoText } from '../i18n.js';
import { CAPS, MIN_VERSION, PROTOCOL_VERSION } from '../proto.js';
import { state, setView } from '../state.js';

const computerUpdate = (d) => d.version && d.version < PROTOCOL_VERSION
  ? t(d.version < MIN_VERSION ? 'update.computerTitle' : 'update.available') : '';

export const statusOf = (d) => {
  const { text, dot } = presenceOf(d);
  const update = computerUpdate(d);
  return { text: update ? `${text} · ${update}` : text, dot };
};

const presenceOf = (d) => {
  if (!d.online) return { text: d.last_seen ? t('status.offlineSince', { ago: agoText(d.last_seen) }) : t('status.offline'), dot: '' };
  // An AI client works beside whoever holds the computer: said too, never someone to take over from.
  const ai = aiUsing(d);
  const also = ai ? ` · ${ai}` : '';
  if (d.controller) return { text: holderText(d.controller) + also, dot: 'warn' };
  return { text: ai ? `${t('status.online')} · ${ai}` : t('status.online'), dot: ai ? 'warn' : 'ok' };
};

// This computer: the computer this browser runs on (here.js); null for any other.
export const hereTag = (d) => (d.here ? h('span', { class: 'tag accent' }, t('here.tag')) : null);

// The live hello supplies the caps. Before it arrives, the desk seat is unknown.
export const noDesktop = (d) => d.online && Array.isArray(d.caps) && !d.caps.includes(CAPS.DESK);
export const showNoDesktop = () => gotIt(t('session.noDesktop'));

// Whether the computer takes AI control: it said which AI tools it carries out (an older
// Deskway says none). Known while it is offline too: the server keeps what it last said.
export const aiSupported = (d) => Array.isArray(d.tools) && d.tools.length > 0;

// The AI client using a computer now, in words; '' when none.
export const aiUsing = (d) => (d && d.ai && d.ai.name ? t('status.aiUsing', { name: d.ai.name }) : '');

// Who holds a computer, from the device list's `controller`: a device of the account, by its
// name; still connecting or in control.
const holderText = (c) => t(c.connecting ? 'status.connecting' : 'status.controlledBy', { name: c.name });

export const statusLine = (d) => {
  const { text, dot } = presenceOf(d);
  const update = computerUpdate(d);
  return h('div', { class: 'computer-status' },
    h('div', { class: 'status' }, h('span', { class: `dot ${dot}`, 'aria-hidden': 'true' }), h('span', {}, text)),
    update ? h('div', { class: 'computer-update' }, update) : null);
};

// The top bar of every page: Deskway at the left; at the right what this page has (the language
// on top-level pages, the account once signed in).
export const topbar = (...end) => h('header', { class: 'topbar' }, h('div', { class: 'brand' }, 'Deskway'), h('div', { class: 'topbar-end' }, ...end));

// The page's title, at the same place on every page. `back`: a page under My computers starts its
// title with the way back, a mark in the title's own type; only the mark goes back (to My computers,
// or to the page `back` names), the title stays text. `beside`: what follows the title (a count); `end`: what sits at the row's far end
// (a computer's ⋯); `sub`: a line under the title, starting where the title starts.
export const heading = (title, { back = false, beside = null, end = null, sub = null } = {}) =>
  h('div', { class: 'heading' },
    h('div', { class: 'heading-row' },
      h('h1', {},
        back ? h('button', { class: 'title-back', 'aria-label': t(back === true ? 'nav.backToList' : 'nav.back'), onclick: () => setView(back === true ? 'list' : back) }, svg(ICONS.titleBack)) : null,
        h('span', { class: 'title-text' }, title)),
      beside,
      end ? h('div', { class: 'heading-end' }, end) : null),
    sub);

// A newer Deskway is on the server: reloading takes it.
export const updateNote = () => (state.updateReady ? h('div', { class: 'notice', role: 'status' },
  h('span', { class: 'dot accent', 'aria-hidden': 'true' }), h('span', { class: 'text' }, t('update.ready')),
  h('button', { class: 'btn compact', onclick: () => location.reload() }, t('update.reload'))) : null);

// A line of state on a page that is still being brought up to date.
export const staleNote = (key) => h('div', { class: 'notice', role: 'status' }, spinner(), h('span', { class: 'text' }, t(key)));

// One setting: what it is, a line under it (text or an element), and its control at the end.
export const settingRow = (label, sub, control) => h('div', { class: 'setting-row' },
  h('div', {}, h('div', {}, label), h('div', { class: 'device-sub' }, sub)),
  control);

// Six digits, as "123 456"; `label` is what a screen reader says instead.
export const codeBox = (code, label) => h('div', { class: 'code', 'aria-label': label }, `${code.slice(0, 3)} ${code.slice(3)}`);

export const copyableCode = (text) => h('code', { class: 'copyable-code' }, text);

export function thumbFor(d) {
  const src = state.thumbs[d.id];
  const inner = h('div', { class: 'thumb-inner' });
  if (!d.online) {
    inner.append(t('thumb.none'));
    return inner;
  }
  const img = h('img', { alt: '', 'data-thumb': d.id, hidden: !src });
  if (src) img.src = src;
  inner.append(img);
  if (!src) inner.append(h('span', { 'data-placeholder': d.id }, t('thumb.loading')));
  return inner;
}

// The language, in the top bar of a top-level page: a menu like the account's. Each language is
// listed in its own name, whatever the page's language; switching shows it at once (`redraw`
// draws the open page again).
export function languagePicker(redraw) {
  const now = currentLang();
  const button = h('button', { class: 'btn text bar-btn lang-btn', 'data-lang': now, 'aria-label': `${t('lang.label')}: ${t(`lang.${now}`)}` },
    svg(ICONS.globe), h('span', { class: 'lang-name' }, t(`lang.${now}`)), svg(ICONS.chevron));
  return dropdown('lang', button, (item) => LANGS.map((l) => item(t(`lang.${l}`), async () => {
    await setLanguage(l, { save: true });
    redraw();
  }, { checked: l === now, lang: l }))).el;
}
