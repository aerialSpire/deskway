// The two places the page offers an action on a kept terminal.
import { CAPS } from './proto.js';

export function keptTerminalNotice(d, t, ago) {
  const kept = d.kept_terminal;
  if (!d.online || !d.caps?.includes(CAPS.TERMINAL) || !kept) return null;
  return t('terminal.kept', { name: kept.name, ago: ago(kept.since) });
}

export function terminalDisconnectActions(t, cancel, close, leave, canClose) {
  return [
    { label: t('common.cancel'), run: cancel },
    { label: t('terminal.closeAndDisconnect'), danger: true, disabled: !canClose, run: close },
    { label: t('tb.disconnect'), primary: true, run: leave },
  ];
}
