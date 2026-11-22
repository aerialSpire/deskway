// A menu that drops from the button that opens it (design.css .dropdown, .menu): the account's
// under the name in the top bar, a computer's under its ⋯. Its items are made when it opens,
// so they always say what is true now. A press outside it, Esc or choosing an item closes it;
// the page being drawn again under it (the server telling of a change) does not.
import { h, svg, ICONS } from '../dom.js';

// The open menu: its key, its element, and how to close it.
let current = null;

// `key` names the menu across redraws ('account', 'computer:<id>', 'lang'). `items(item)`
// returns the menu's contents; `item(label, run, { danger, checked, lang })` makes an entry that
// closes the menu and runs. `checked` (true or false) makes it one of a set of choices, the
// chosen one marked; `lang` is the language its words are in.
// Returns the element to place and `open`, for ways other than the button (holding a card).
export function dropdown(key, button, items) {
  const menu = h('div', { class: 'menu', role: 'menu', hidden: true });
  const wrap = h('div', { class: 'dropdown' }, button, menu);
  const item = (label, run, { danger = false, checked = null, lang = null } = {}) => h('button', {
    class: `menu-item ${danger ? 'danger' : ''}`,
    role: checked === null ? 'menuitem' : 'menuitemradio',
    'aria-checked': checked === null ? null : String(checked),
    lang,
    onclick: () => { close(); run(); },
  }, h('span', {}, label), checked ? svg(ICONS.check) : null);
  const outside = (e) => { if (!wrap.contains(e.target)) close(); };
  const esc = (e) => { if (e.key === 'Escape') { close(); button.focus(); } };
  const close = () => {
    if (menu.hidden) return;
    menu.hidden = true;
    button.setAttribute('aria-expanded', 'false');
    document.removeEventListener('pointerdown', outside, true);
    document.removeEventListener('keydown', esc);
    if (current && current.wrap === wrap) current = null;
  };
  const open = () => {
    if (!menu.hidden) return;
    current?.close();
    menu.replaceChildren(...items(item).filter(Boolean));
    menu.hidden = false;
    button.setAttribute('aria-expanded', 'true');
    document.addEventListener('pointerdown', outside, true);
    document.addEventListener('keydown', esc);
    current = { key, wrap, close };
  };
  button.setAttribute('aria-haspopup', 'menu');
  button.setAttribute('aria-expanded', 'false');
  button.addEventListener('click', () => (menu.hidden ? open() : close()));
  // Drawn again while its earlier self is open: once placed, it takes over, open.
  if (current && current.key === key) {
    queueMicrotask(() => { if (current && current.key === key && !current.wrap.isConnected && wrap.isConnected) open(); });
  }
  return { el: wrap, open };
}

export const menuSeparator = () => h('div', { class: 'menu-sep' });
