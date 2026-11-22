// The language menu of the host's windows (the panel, setup), made like the browser client's: a
// globe button that drops the languages, each in its own name, the current one checked
// (design.css .dropdown, .menu, .bar-btn, .lang-btn). ui::designed puts it into each page that
// has the language-menu marker (ui::LANG_MARK), so every window has this one copy.
//
// langMenu(holder, codes, pick) fills `holder` and calls pick(code) when another language is
// chosen; its button has the size of the window bar's own buttons (compact). It returns
// show(code, label, names): draws the current language, the button's accessible label and every
// language's own name.
function langMenu(holder, codes, pick) {
  const GLOBE = '<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2" aria-hidden="true"><circle cx="8" cy="8" r="6.2"/><path d="M1.8 8h12.4M8 1.8c1.7 1.8 2.6 3.9 2.6 6.2S9.7 12.4 8 14.2C6.3 12.4 5.4 10.3 5.4 8S6.3 3.6 8 1.8z"/></svg>';
  const CHEVRON = '<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.2" aria-hidden="true"><path d="M2 3.5 5 6.5 8 3.5"/></svg>';
  const CHECK = '<svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="M2.5 7.2l3 3 6-6.4"/></svg>';
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'btn text compact bar-btn lang-btn';
  button.setAttribute('aria-haspopup', 'menu');
  button.setAttribute('aria-expanded', 'false');
  button.innerHTML = `${GLOBE}<span class="lang-name"></span>${CHEVRON}`;
  const menu = document.createElement('div');
  menu.className = 'menu';
  menu.setAttribute('role', 'menu');
  menu.hidden = true;
  holder.classList.add('dropdown');
  holder.replaceChildren(button, menu);
  let current = codes[0];
  let names = {};
  const close = () => {
    menu.hidden = true;
    button.setAttribute('aria-expanded', 'false');
  };
  const entry = (code) => {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'menu-item';
    b.lang = code;
    b.setAttribute('role', 'menuitemradio');
    b.setAttribute('aria-checked', String(code === current));
    const words = document.createElement('span');
    words.textContent = names[code] || code;
    b.append(words);
    if (code === current) b.insertAdjacentHTML('beforeend', CHECK);
    b.addEventListener('click', () => { close(); if (code !== current) pick(code); });
    return b;
  };
  button.addEventListener('click', () => {
    if (!menu.hidden) { close(); return; }
    menu.replaceChildren(...codes.map(entry));
    menu.hidden = false;
    button.setAttribute('aria-expanded', 'true');
  });
  document.addEventListener('pointerdown', (e) => { if (!holder.contains(e.target)) close(); }, true);
  // Esc closes the menu first, before the page's own Esc (going back, closing a dialog).
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && !menu.hidden) { close(); button.focus(); e.stopImmediatePropagation(); }
  }, true);
  return (code, label, own) => {
    current = code;
    names = own;
    button.querySelector('.lang-name').textContent = own[code] || code;
    button.setAttribute('aria-label', `${label}: ${own[code] || code}`);
  };
}
