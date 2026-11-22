// The session's toolbar: it floats above the screen and stays out of the way (a small tab at
// the top brings it down), shows how the connection goes, opens the panels under it, and
// switches full screen.
//
// Its buttons are words, never wrapped. A wide window: all in one row; where that row does not
// fit, the buttons' words in the small size. A narrow one: who and how in the first row, the
// buttons in the second, sliding sideways when they do not fit (the cut side fades). An AI
// client controlling the computer too is said in a line along its bottom.
//
// `picture`: the controls of the picture itself (Sharp / Smooth), by the connection status
// rather than among the input buttons: in a narrow window those fill their own row.
//
// Public surface: el, tab, panels, panelKind, isFull, hover (held open while under the mouse),
// setRemoteAvailable(ready), addPanel(kind, button, build), togglePanel(kind), closePanel(focus), refreshPanel(kind), placePanels(),
// show(), hide(), hideSoon(ms), fit(), route(kind, hasDetails), toggleFullscreen(), showAi(device), bind(), destroy().
import { $dialogs, h, ICONS, svg } from '../dom.js';
import { aiUsing } from '../views/common.js';
import { t } from '../i18n.js';

export class Toolbar {
  constructor(s, buttons, picture) {
    this.s = s;
    this.panelKind = null;
    this.isFull = false;
    this.hover = false;
    this.builders = {};
    this.buttons = {};
    this.routeText = h('span', {}, t('route.connecting'));
    this.routeDot = h('span', { class: 'dot', 'aria-hidden': 'true' });
    this.routeArrow = h('span', { class: 'route-arrow', hidden: true }, svg(ICONS.chevron));
    this.routeButton = h('button', {
      class: 'tool status tool-status', type: 'button', disabled: true, 'aria-expanded': 'false',
      'aria-controls': 'connection-details', onclick: () => this.togglePanel('connection'),
    }, this.routeDot, this.routeText, this.routeArrow);
    this.panels = h('div', { class: 'panels' });
    this.fullBtn = h('button', { class: 'tool', 'aria-pressed': 'false', onclick: () => this.toggleFullscreen() }, t('tb.full'));
    this.nameText = h('span', { class: 'tool-name-text', title: s.device.name }, s.device.name);
    // The buttons as one group: in line on a wide window, a row of their own on a narrow one.
    this.remoteButtons = buttons.filter(Boolean);
    this.group = h('div', { class: 'tool-group' }, ...this.remoteButtons, this.fullBtn);
    this.group.addEventListener('scroll', () => this.fade(), { passive: true });
    this.main = h('div', { class: 'tool-main' },
      h('div', { class: 'tool-name' }, this.nameText),
      this.routeButton,
      h('div', { class: 'tool-picture' }, ...picture.filter(Boolean)),
      this.group,
      h('span', { class: 'tool-sep', 'aria-hidden': 'true' }),
      h('button', { class: 'tool end', onclick: () => s.confirmLeave() }, t('tb.disconnect')));
    // An AI client controlling the computer too: who, along the bar's bottom.
    this.aiText = h('span', { class: 'text' });
    this.aiLine = h('div', { class: 'tool-ai', role: 'status', hidden: true }, h('span', { class: 'dot accent', 'aria-hidden': 'true' }), this.aiText);
    this.el = h('div', { class: 'toolbar' }, this.main, this.aiLine);
    // The tab that brings the bar down is always in sight: it says who controls it too.
    this.tabMark = h('span', { class: 'tab-ai', hidden: true });
    this.tab = h('button', { class: 'toolbar-tab', 'aria-label': t('tb.showToolbar'), onclick: () => this.show() }, this.tabMark);
    // Who works here now, to say when that changes.
    this.aiName = '';
  }

  // The device list changed: whether an AI client is using this computer now.
  showAi(device) {
    const text = aiUsing(device);
    const name = (device && device.ai && device.ai.name) || '';
    this.aiText.textContent = name ? t('tb.aiAlso', { name }) : '';
    this.aiText.title = this.aiText.textContent;
    this.aiLine.hidden = !name;
    this.tabMark.textContent = text;
    this.tabMark.title = text;
    this.tabMark.hidden = !text;
    this.tab.classList.toggle('ai', !!text);
    this.tab.setAttribute('aria-label', text ? `${t('tb.showToolbar')} · ${text}` : t('tb.showToolbar'));
    // Its coming and going is said once, over the picture: the bar may be away.
    if (name !== this.aiName) {
      if (name) this.s.note(t('tb.aiStarted', { name }), 5000);
      else this.s.note(t('tb.aiStopped', { name: this.aiName }), 5000);
      this.aiName = name;
    }
  }

  bind() {
    const stage = this.s.stage;
    this.onResize = () => this.fit();
    addEventListener('resize', this.onResize);
    this.tab.addEventListener('pointerenter', (e) => { if (e.pointerType === 'mouse') this.show(); });
    // It slides away again on its own: after a pause, when the mouse leaves it, after a button
    // is used, and at once when the remote screen is touched.
    this.el.addEventListener('pointerenter', (e) => {
      if (e.pointerType !== 'mouse') return;
      this.hover = true;
      clearTimeout(this.timer);
    });
    this.el.addEventListener('pointerleave', (e) => {
      if (e.pointerType !== 'mouse') return;
      this.hover = false;
      this.hideSoon(800);
    });
    this.el.addEventListener('click', (e) => {
      // Touch has no "leave", so a tap counts as being done with it.
      if (e.pointerType !== 'mouse') this.hover = false;
      this.hideSoon(1500);
    });
    stage.addEventListener('pointerdown', () => {
      if (this.el.classList.contains('shown')) this.hide();
      // A floating panel closes when the screen is used.
      this.closePanel(this.s.stage);
    });
    this.onKey = (e) => {
      if (e.key !== 'Escape' || this.panelKind !== 'connection') return;
      e.preventDefault();
      e.stopPropagation();
      this.closePanel(this.routeButton);
    };
    this.s.root.addEventListener('keydown', this.onKey, true);
    this.onFullscreenChange = () => this.syncFullscreen();
    document.addEventListener('fullscreenchange', this.onFullscreenChange);
  }

  setRemoteAvailable(ready) {
    for (const button of this.remoteButtons) button.disabled = !ready;
    if (this.buttons[this.panelKind]?.disabled) this.closePanel(this.s.stage);
  }

  // A panel opened by `button`, its contents made by `build()` each time it opens.
  addPanel(kind, button, build) {
    this.buttons[kind] = button;
    this.builders[kind] = build;
  }

  togglePanel(kind) {
    const button = this.buttons[kind];
    if (!button || button.disabled) return;
    if (this.panelKind === kind) {
      this.closePanel(kind === 'connection' ? button : this.s.stage);
      return;
    }
    this.closePanel();
    this.panelKind = kind;
    this.panelExpanded(kind, true);
    this.placePanels();
    this.panels.append(this.builders[kind]());
  }

  closePanel(focus) {
    if (!this.panelKind) return;
    if (this.panelKind === 'connection') this.hideSoon(1500);
    this.panelExpanded(this.panelKind, false);
    this.panels.replaceChildren();
    this.panelKind = null;
    focus?.focus({ preventScroll: true });
  }

  panelExpanded(kind, open) {
    const button = this.buttons[kind];
    button.setAttribute(button.hasAttribute('aria-expanded') ? 'aria-expanded' : 'aria-pressed', String(open));
  }

  // Draws an open panel again, with what changed.
  refreshPanel(kind) {
    if (this.panelKind === kind) this.panels.replaceChildren(this.builders[kind]());
  }

  // Panels open under the toolbar, which is two rows high in a narrow window: its height is
  // given to style.css (.panels), which places them.
  placePanels() {
    this.panels.style.setProperty('--toolbar-h', `${this.el.offsetHeight}px`);
  }

  show() {
    this.el.classList.add('shown');
    this.fit();
    this.hideSoon(3000);
  }

  // A wide window: the buttons' words go small when the row does not fit with the computer's
  // name given a few words; a narrow one slides its buttons instead (style.css decides which).
  fit() {
    this.el.classList.remove('small');
    if (getComputedStyle(this.main).display !== 'grid') {
      const cut = this.nameText.scrollWidth > this.nameText.clientWidth;
      if (this.main.scrollWidth > this.main.clientWidth + 1 || (cut && this.nameText.clientWidth < 6 * parseFloat(getComputedStyle(this.nameText).fontSize))) this.el.classList.add('small');
    }
    this.fade();
  }

  // Buttons beyond either end of their row: that end fades out.
  fade() {
    const g = this.group;
    const more = g.scrollWidth > g.clientWidth + 1;
    g.classList.toggle('fade-start', more && g.scrollLeft > 1);
    g.classList.toggle('fade-end', more && g.scrollLeft + g.clientWidth < g.scrollWidth - 1);
  }

  hideSoon(ms) {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      // Still under the mouse: wait for it to leave, which hides it again.
      if (this.hover || this.panelKind === 'connection') return;
      this.hide();
    }, ms);
  }

  hide() {
    clearTimeout(this.timer);
    this.el.classList.remove('shown');
    if (this.el.contains(document.activeElement)) this.s.stage.focus({ preventScroll: true });
  }

  route(kind, hasDetails = false) {
    const label = t(`route.${kind}`);
    if (this.routeText.textContent === label && this.routeButton.disabled === !hasDetails) return;
    if (!hasDetails && this.panelKind === 'connection') this.closePanel(this.s.stage);
    this.routeText.textContent = label;
    this.routeDot.className = kind === 'connecting' ? 'dot' : kind === 'reconnecting' ? 'dot warn' : 'dot ok';
    this.routeButton.disabled = !hasDetails;
    this.routeArrow.hidden = !hasDetails;
    this.routeButton.setAttribute('aria-label', hasDetails ? t('connectionDetails.open', { route: label }) : label);
    if (this.el.classList.contains('shown')) this.fit();
  }

  async toggleFullscreen() {
    const root = this.s.root;
    if (this.isFull) {
      this.isFull = false;
      if (document.fullscreenElement) await document.exitFullscreen().catch(() => {});
      this.applyFull();
      return;
    }
    this.isFull = true;
    // Browsers without element full screen, like Safari on iPhone, still get the toolbar out of the way.
    if (root.requestFullscreen) {
      await root.requestFullscreen({ navigationUI: 'hide' }).catch(() => {});
      // Sends Esc and system shortcuts to the remote computer. Holding Esc still exits.
      if (document.fullscreenElement && navigator.keyboard && navigator.keyboard.lock) {
        navigator.keyboard.lock().catch(() => {});
      }
    }
    this.applyFull();
  }

  // Leaving full screen from the browser itself, for example by holding Esc.
  syncFullscreen() {
    if (!document.fullscreenElement && this.isFull) {
      this.isFull = false;
      this.applyFull();
    }
  }

  applyFull() {
    const root = this.s.root;
    // Dialogs (the question before disconnecting) show over the session in full screen.
    if (this.isFull && document.fullscreenElement === root) root.append($dialogs);
    else if ($dialogs.parentElement !== document.body) document.body.append($dialogs);
    this.fullBtn.textContent = this.isFull ? t('tb.exitFull') : t('tb.full');
    this.fullBtn.setAttribute('aria-pressed', this.isFull ? 'true' : 'false');
    if (this.panelKind === 'connection') this.closePanel();
    this.el.classList.remove('shown');
    if (!this.isFull && navigator.keyboard && navigator.keyboard.unlock) navigator.keyboard.unlock();
    this.s.stage.focus();
  }

  destroy() {
    clearTimeout(this.timer);
    removeEventListener('resize', this.onResize);
    this.s.root.removeEventListener('keydown', this.onKey, true);
    document.removeEventListener('fullscreenchange', this.onFullscreenChange);
    if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
    if (navigator.keyboard && navigator.keyboard.unlock) navigator.keyboard.unlock();
  }
}
