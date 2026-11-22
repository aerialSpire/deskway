// Dialogs: one at a time, in the dialog layer over the page (or over the session in full
// screen). When one closes, whatever waits for the layer (an approval) gets its turn.
import { $dialogs, h } from './dom.js';
import { t } from './i18n.js';
import { signedOut } from './net.js';

let afterClose = () => {};

// Called after a dialog is answered or cancelled.
export function onDialogClosed(fn) { afterClose = fn; }

export const closeDialogs = () => $dialogs.replaceChildren();
export const dialogOpen = () => $dialogs.childElementCount > 0;

// `text` is a string or an element; each action is { label, primary, run }.
export function dialog(title, text, actions) {
  $dialogs.replaceChildren(h('div', { class: 'overlay' },
    h('div', { class: 'dialog', role: 'dialog', 'aria-modal': 'true', 'aria-label': title },
      h('h2', {}, title),
      typeof text === 'string' ? h('p', {}, text) : text,
      h('div', { class: 'dialog-actions' }, actions.filter(Boolean).map((a) => h('button', {
        class: `btn ${a.primary ? 'primary' : ''}`,
        onclick: () => { closeDialogs(); if (a.run) a.run(); afterClose(); },
      }, a.label))))));
  return $dialogs.firstElementChild;
}

// A dialog that only tells something, closed with “OK”; `primary` makes that the main
// button, `also` is one more action beside it.
export const gotIt = (title, text, { primary = false, also = null } = {}) =>
  dialog(title, text, [{ label: t('common.gotIt'), primary }, also]);

// A small form in a dialog: fields, an error line, and a submit that may fail and stay open.
// `submit` returns { title, text } to show when done, a function that opens the dialog that
// follows, or nothing to just close.
export function formDialog(title, intro, fields, submitLabel, submit) {
  const error = h('div', { class: 'error', role: 'alert' });
  const go = h('button', { class: 'btn primary', type: 'submit' }, submitLabel);
  const form = h('form', {
    class: 'dialog form', role: 'dialog', 'aria-modal': 'true', 'aria-label': title,
    onsubmit: async (e) => {
      e.preventDefault();
      error.textContent = '';
      go.disabled = true;
      try {
        const done = await submit();
        closeDialogs();
        if (typeof done === 'function') done();
        else if (done) gotIt(done.title, done.text);
        else afterClose();
      } catch (err) {
        // Signed out: the sign-in page has taken the dialog away. Otherwise only messages
        // written for the person are shown; anything else (a network error) is said in general.
        if (signedOut(err)) return;
        error.textContent = err.shown ? err.message : t('err.noServer');
      }
      go.disabled = false;
    },
  },
  h('h2', {}, title),
  intro ? h('p', {}, intro) : null,
  h('div', { class: 'fields' }, fields),
  error,
  h('div', { class: 'dialog-actions' },
    h('button', { class: 'btn', type: 'button', onclick: () => { closeDialogs(); afterClose(); } }, t('common.cancel')),
    go));
  $dialogs.replaceChildren(h('div', { class: 'overlay' }, form));
  form.querySelector('input, textarea')?.focus();
}

// An error whose message is meant for the person, as opposed to one from the browser.
export const shownError = (key) => Object.assign(new Error(t(key)), { shown: true });

// A name to choose (a computer's, a device's): `save(name)` runs for a new one and says
// whether it was saved.
export function nameDialog(title, label, current, save) {
  const input = h('input', { class: 'input', value: current, maxlength: 40, 'aria-label': label });
  formDialog(title, null, [h('label', { class: 'field-label' }, t('common.name'), input)], t('common.save'), async () => {
    const name = input.value.trim();
    if (!name) throw shownError('common.nameEmpty');
    if (name !== current && !(await save(name))) throw shownError('err.notSaved');
    return null;
  });
  input.select();
}
