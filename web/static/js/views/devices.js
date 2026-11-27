// My devices: every device on the account, in two groups: the ones used to control (this phone,
// a laptop) and the computers being controlled, each group one card. A device's actions are
// under its ⋯, whatever the language or the width; a computer's AI settings sit in its card.
// Then the account's security setting and this device's own settings.
import { $app, h, svg, ICONS, loading } from '../dom.js';
import { store } from '../env.js';
import { t, agoText, currentLang } from '../i18n.js';
import { api, signedOut } from '../net.js';
import { signWith, setPolicy, setAi, storedPasskey, keyPersisted, renameDevice } from '../auth.js';
import { removeChallenge } from '../auth-messages.js';
import { state } from '../state.js';
import { dialog, gotIt, nameDialog } from '../dialogs.js';
import { statusOf, topbar, heading, updateNote, settingRow, aiSupported } from './common.js';
import { openAiClient } from './ai-client.js';
import { dropdown, moreButton } from './menu.js';
import { PICTURES, picture, setPicture } from '../picture.js';
import { accountMenu } from './account.js';
import { computerMenu } from './computer.js';
import { aiDeviceTag } from '../mcp-approval.js';

const controllerIcon = (name) => svg(/iPhone|iPad|Android/.test(name || '') ? ICONS.phone : ICONS.laptop);

// Opened, the page says it is loading. Already showing, it only brings its contents up to date:
// it stays as it is while the new contents load, keeps its scroll position, and is left alone
// when nothing it shows has changed.
let seq = 0;
let shown = '';
export async function renderDevices() {
  const mine = ++seq;
  const showing = !!$app.querySelector('.page[data-page="devices"]');
  const page = (...body) => {
    const y = scrollY;
    $app.replaceChildren(h('div', { class: 'page', 'data-page': 'devices' }, topbar(accountMenu()), updateNote(), heading(t('devices.title'), { back: true }), ...body));
    if (showing) scrollTo(0, y);
  };
  if (!showing) { shown = ''; page(loading(t('common.loading'))); }
  let rows;
  let policy = { keys: true };
  let kept = null;
  try {
    const [res, pol, persisted] = await Promise.all([api('GET', '/api/devices'), api('GET', '/api/policy'), keyPersisted()]);
    kept = persisted;
    if (!res.ok) throw new Error(String(res.status));
    rows = await res.json();
    if (pol.ok) policy = await pol.json();
  } catch (e) {
    if (mine !== seq || signedOut(e)) return;
    shown = '';
    page(h('p', { class: 'hint' }, t('devices.loadFailed')));
    return;
  }
  // A newer call is under way, or the person has left the page.
  if (mine !== seq || state.view !== 'devices') return;
  const now = JSON.stringify([rows, policy, state.devices.map((d) => [d.id, d.name, statusOf(d).text, d.ai]), store.get('deskway.autofull'), store.get('deskway.windows'), picture(), currentLang(), kept, state.me.display, state.updateReady]);
  if (now === shown && $app.querySelector('.page[data-page="devices"]')) return;
  shown = now;
  // A device: its name alone on the first line (… when long), then its tags and its state, which
  // may take a second line; its ⋯ at the end.
  const row = (icon, name, tags, sub, menu) => h('div', { class: 'device-row' },
    h('div', { class: 'device-icon' }, icon),
    h('div', { class: 'device-text' },
      h('div', { class: 'device-name' }, h('span', { class: 'name', title: name }, name)),
      h('div', { class: 'device-sub' }, ...tags.map(([text, cls]) => h('span', { class: `tag ${cls || ''}` }, text)), sub)),
    h('div', { class: 'device-actions' }, menu));

  const controllers = rows.map((r) => {
    const name = (r.device && r.device.name) || r.label;
    const ai = aiDeviceTag(r);
    const label = r.current ? t('devices.signOut') : t('common.remove');
    const menu = dropdown(`browser:${r.id}`, moreButton(t('computer.menu', { name })),
      (item) => [
        item(t('common.rename'), () => nameDialog(t('devices.renameTitle'), t('devices.nameLabel'), name, async (n) => {
          const ok = await renameDevice(r.id, r.device, n);
          if (ok) renderDevices();
          return ok;
        })),
        item(label, () => confirmRemove(r, name), { danger: true }),
      ]);
    return row(controllerIcon(name), name,
      [ai ? [t(ai), 'accent'] : null, r.current ? [t('devices.tagThis'), ''] : null].filter(Boolean),
      r.current ? t('devices.inUse') : t('devices.lastUsed', { when: agoText(r.last_seen) || t('devices.longAgo') }),
      menu.el);
  });
  // The way to connect an AI client, last in the card of the devices that control.
  const addAi = h('button', { class: 'device-add', type: 'button', onclick: openAiClient },
    h('div', { class: 'device-icon' }, svg(ICONS.plus)),
    h('div', { class: 'device-text' }, h('div', { class: 'device-name link' }, t('devices.addAI')), h('div', { class: 'device-sub' }, t('devices.addAISub'))),
    h('span', { class: 'device-go', 'aria-hidden': 'true' }, '›'));
  const computers = state.devices.map((d) => {
    const s = statusOf(d);
    return h('div', { class: 'device-item' },
      row(svg(ICONS.screen), d.name, [],
        h('span', { class: 'device-state' }, h('span', { class: `dot ${s.dot}`, 'aria-hidden': 'true' }),
          d.ai && d.ai.state === 'stopped' ? `${s.text} · ${t('devices.aiStopped')}` : s.text),
        computerMenu(d).el),
      aiSettings(d));
  });

  const toggle = h('button', { class: 'switch', role: 'switch', 'aria-checked': String(policy.keys), 'aria-label': t('devices.checks') }, h('span'));
  toggle.addEventListener('click', () => {
    const turnOff = policy.keys;
    const go = async () => {
      try {
        await setPolicy(!turnOff);
        renderDevices();
      } catch (e) {
        if (!signedOut(e)) gotIt(t('devices.notChanged'), t('devices.onlyAdded'));
      }
    };
    if (turnOff) {
      dialog(t('devices.offTitle'), t('devices.offText'), [
        { label: t('common.cancel') },
        { label: t('devices.turnOff'), primary: true, run: go },
      ]);
    } else {
      go();
    }
  });

  // A setting of this device only, kept in this browser: on while `deskway.<key>` is '1' (or,
  // for one on by default, anything but '0').
  const localSetting = (key, defaultOn) => {
    const isOn = () => (defaultOn ? store.get(`deskway.${key}`) !== '0' : store.get(`deskway.${key}`) === '1');
    const text = (on) => t(`devices.${key}${on ? 'On' : 'Off'}`);
    const sub = h('span', {}, text(isOn()));
    const toggle = h('button', { class: 'switch', role: 'switch', 'aria-checked': String(isOn()), 'aria-label': t(`devices.${key}`),
      onclick: () => {
        const on = !isOn();
        store.set(`deskway.${key}`, on ? '1' : '0');
        toggle.setAttribute('aria-checked', String(on));
        sub.textContent = text(on);
        shown = '';
      } }, h('span'));
    return settingRow(t(`devices.${key}`), sub, toggle);
  };

  // How the remote screen comes: sharper (the default) or smoother.
  const chosen = picture();
  const pictureMenu = dropdown('picture', h('button', { class: 'btn compact', 'aria-label': `${t('devices.picture')}: ${t(`picture.${chosen}`)}` },
    t(`picture.${chosen}`), svg(ICONS.chevron)), (item) => PICTURES.map((p) => item(t(`picture.${p}`), () => {
    setPicture(p);
    renderDevices({ quiet: true });
  }, { checked: p === chosen }))).el;

  page(
    h('h2', { class: 'section-title' }, t('devices.controllers'), h('span', {}, t('devices.controllersSub'))),
    h('div', { class: 'device-list grouped' }, controllers, addAi),
    h('p', { class: 'hint add-hint' }, t('devices.addController', { host: location.host })),
    h('h2', { class: 'section-title' }, t('devices.computers'), h('span', {}, t('devices.computersSub'))),
    computers.length ? h('div', { class: 'device-list grouped' }, computers) : h('p', { class: 'hint' }, t('devices.noComputers')),
    h('p', { class: 'hint add-hint' }, t('devices.addComputer')),
    h('h2', { class: 'section-title' }, t('devices.security')),
    settingRow(t('devices.checks'), policy.keys ? t('devices.checksOn') : t('devices.checksOff'), toggle),
    h('h2', { class: 'section-title' }, t('devices.thisDevice')),
    // The browser refused to keep this site's storage: its device key may be deleted.
    kept === false && storedPasskey() ? h('p', { class: 'hint key-hint' }, t('devices.keyNotKept')) : null,
    settingRow(t('devices.picture'), t(`picture.${chosen}Sub`), pictureMenu),
    localSetting('autofull', true),
    localSetting('windows', false));
}

// Whether AI clients may control this computer, and with administrator rights: off until a
// browser of the account turns it on, one signed switch for both (docs/architecture.md, "The
// AI switch"). Stopped at the computer shows as off: turning it on again signs a newer switch,
// which opens it. Administrator rights are offered only while AI is allowed, and go with it.
// Each is a setting's row in the computer's card: what it allows, said under it.
function aiSettings(d) {
  if (!aiSupported(d)) return h('div', { class: 'device-settings' }, h('p', { class: 'device-note' }, t('devices.aiOld')));
  const on = !!d.ai && d.ai.state === 'on';
  const admin = on && !!d.ai.admin;
  const set = async (ai, withAdmin) => {
    try {
      await setAi(d.id, ai, withAdmin, (d.ai && d.ai.ts) || 0);
    } catch (e) {
      if (!signedOut(e)) gotIt(t('devices.notChanged'), t('devices.onlyAdded'));
    }
  };
  // A switch that asks first before it turns on.
  const toggle = (label, sub, checked, data, ask, turn) => {
    const button = h('button', { class: 'switch', role: 'switch', 'aria-checked': String(checked), 'aria-label': `${label}: ${d.name}`, ...data }, h('span'));
    button.addEventListener('click', () => {
      if (checked) { turn(false); return; }
      dialog(ask.title, ask.text, [{ label: t('common.cancel') }, { label: t('devices.allow'), primary: true, run: () => turn(true) }]);
    });
    return settingRow(label, sub, button);
  };
  const ai = toggle(t('devices.allowAI'), t('devices.allowAISub'), on, { 'data-ai': d.id }, { title: t('devices.aiOnTitle', { name: d.name }), text: t('devices.aiOnText') }, (yes) => set(yes, false));
  const rights = on ? toggle(t('devices.allowAdmin'), t('devices.allowAdminSub'), admin, { 'data-ai-admin': d.id }, { title: t('devices.adminOnTitle', { name: d.name }), text: t('devices.adminOnText') }, (yes) => set(true, yes)) : null;
  // The computer did not take the latest switch: no device it trusts signed it. It takes it by
  // itself once one does, so the person is told what to do there instead of trying again here.
  const refused = d.ai && d.ai.refused ? h('p', { class: 'device-note', 'data-ai-refused': d.id }, t('devices.aiRefused', { name: d.name })) : null;
  return h('div', { class: 'device-settings' }, ai, rights, refused);
}

function confirmRemove(r, name) {
  const removeIt = async () => {
    let proof;
    try {
      // Signed, so the computers stop trusting it too, not only this website.
      if (r.device) proof = await signWith((ts) => removeChallenge(r.device.cred, ts));
    } catch { /* this browser has no key: the website still signs it out */ }
    let ok = false;
    try {
      ok = (await api('DELETE', `/api/browsers/${r.id}`, { proof })).ok;
    } catch (e) {
      if (signedOut(e)) return;
    }
    if (r.current) { location.reload(); return; }
    if (ok) renderDevices();
    else gotIt(t('devices.notRemoved'), t('err.checkNetwork'));
  };
  dialog(r.current ? t('devices.signOutTitle') : t('devices.removeTitle', { name }),
    r.current ? t('devices.signOutText') : t('devices.removeText'), [
      { label: t('common.cancel') },
      { label: r.current ? t('devices.signOut') : t('common.remove'), primary: true, run: removeIt },
    ]);
}
