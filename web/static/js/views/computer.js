// A computer's own menu under its ⋯ (on its card in the list, beside its name on its page):
// rename it, or take it off the account (also when it is offline for good). Holding its card
// opens the same menu. My devices renames and removes with the same two actions.
import { t } from '../i18n.js';
import { api, signedOut } from '../net.js';
import { state, setView, render } from '../state.js';
import { dialog, gotIt, nameDialog } from '../dialogs.js';
import { dropdown, menuSeparator, moreButton } from './menu.js';

// Returns the ⋯ to place and `open`.
export function computerMenu(d) {
  return dropdown(`computer:${d.id}`, moreButton(t('computer.menu', { name: d.name })), (item) => [
    item(t('common.rename'), () => renameComputer(d)),
    menuSeparator(),
    item(t('computer.removeFromAccount'), () => confirmRemoveComputer(d), { danger: true }),
  ]);
}

export function renameComputer(d) {
  nameDialog(t('computer.renameTitle'), t('computer.nameLabel'), d.name, async (name) => {
    const res = await api('PATCH', `/api/computers/${d.id}`, { name });
    if (res.ok) render();
    return res.ok;
  });
}

export function confirmRemoveComputer(d) {
  dialog(t('computer.removeTitle', { name: d.name }), t('computer.removeText'), [
    { label: t('common.cancel') },
    { label: t('common.remove'), primary: true, run: async () => {
      try {
        const res = await api('DELETE', `/api/computers/${d.id}`);
        if (!res.ok) throw new Error(String(res.status));
      } catch (e) {
        if (!signedOut(e)) gotIt(t('devices.notRemoved'), t('err.checkNetwork'));
        return;
      }
      if (state.view === 'devices') render();
      else setView('list');
    } },
  ]);
}
