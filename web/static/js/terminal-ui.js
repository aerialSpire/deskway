// Terminal notices and actions.
import { CAPS } from './proto.js';

export function keptTerminalNotice(d, t, ago) {
  const kept = d.kept_terminal;
  if (!d.online || !d.caps?.includes(CAPS.TERMINAL) || d.caps.includes(CAPS.TERMINALS) || !kept) return null;
  return t('terminal.kept', { name: kept.name, ago: ago(kept.since) });
}

export function terminalActions(device, t, ago, open, current) {
  const rows = (device?.terminals ?? []).map(({ id, name, left, attached }) => {
    return {
      text: attached ? t('terminal.attached', { id, name }) : t('terminal.detached', { id, name, ago: ago(left) }),
      label: t(id === current ? 'terminal.current' : 'terminal.return'),
      disabled: id === current,
      run: () => open(id),
    };
  });
  return { rows, newTerminal: { label: t('terminal.new'), run: () => open(null) } };
}

export function terminalDisconnectActions(t, cancel, close, leave, canClose) {
  return [
    { label: t('common.cancel'), run: cancel },
    { label: t('terminal.closeAndDisconnect'), danger: true, disabled: !canClose, run: close },
    { label: t('tb.disconnect'), primary: true, run: leave },
  ];
}
