// Terminal notices and actions.
import { CAPS } from './proto.js';

export function keptTerminalNotice(d, t, ago) {
  const kept = d.kept_terminal;
  if (!d.online || !d.caps?.includes(CAPS.TERMINAL) || d.caps.includes(CAPS.TERMINALS) || !kept) return null;
  return t('terminal.kept', { name: kept.name, ago: ago(kept.since) });
}

export function terminalActions(device, t, ago, open, current, takeover) {
  const rows = (device?.terminals ?? []).map(({ id, name, left, attached, here }) => {
    if (attached && here && takeover) {
      return { text: t('terminal.here', { id }), label: t('terminal.takeOver'), run: () => open(id, true) };
    }
    const text = attached ? t('terminal.attached', { id, name })
      : here ? t('terminal.detachedHere', { id, ago: ago(left) })
        : t('terminal.detached', { id, name, ago: ago(left) });
    return {
      text,
      label: t(id === current ? 'terminal.current' : 'terminal.return'),
      disabled: id === current,
      run: () => open(id),
    };
  });
  return { rows, newTerminal: { label: t('terminal.new'), run: () => open(null) } };
}

export function terminalTakenNotice(t, id) {
  return t('terminal.takenHere', { id });
}

export function terminalDisconnectActions(t, cancel, close, leave, canClose) {
  return [
    { label: t('common.cancel'), run: cancel },
    { label: t('terminal.closeAndDisconnect'), danger: true, disabled: !canClose, run: close },
    { label: t('tb.disconnect'), primary: true, run: leave },
  ];
}
