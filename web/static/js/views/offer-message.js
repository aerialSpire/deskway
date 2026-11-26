// What the desk seat's offer refusals and takeover show, independent of the dialog layer.
export function takeoverMessage(c, t) {
  if (c.device && c.connecting) return { action: 'replace' };
  if (c.device) return { action: 'takeover', title: t('force.deviceTitle'), text: t('force.deviceText'), go: t('force.deviceGo') };
  return {
    action: 'takeover',
    title: t(c.connecting ? 'force.accountConnecting' : 'force.account', { name: c.name }),
    text: t('force.accountText'),
    go: t('force.accountGo'),
  };
}

export function workspaceActions(d, t) {
  const notice = d.online ? workspaceNotice(d.workspace_access, t) : undefined;
  const available = !!d.online && (!notice || d.workspace_access === 'locked');
  return { notice, available, disabled: Boolean(notice) };
}

export function workspaceNotice(reason, t) {
  switch (reason) {
    case 'locked': return t('detail.workspaceLocked');
    case 'no_user_session': return t('detail.workspaceNoUser');
    case 'view_only': return t('audit.code.view_only');
  }
}

export function offerMessage({ reason, by, same, controller, established, cause, via }, { view, headless, t, time }) {
  const endedText = (desktop) => t(view === 'terminal' ? 'terminal.ended' : view === 'files' ? 'files.ended' : desktop);
  switch (reason) {
    case 'failed':
    case 'gone':
      if (!established) return { action: 'retry', title: t('session.notConnected'), text: t('session.notConnectedText') };
      if (cause === 'host') return { action: 'reconnect', title: t('session.offlineTitle'), text: endedText('session.offlineText') };
      return { action: 'reconnect', title: t('session.failedTitle'), text: endedText('session.failedText') };
    case 'offline':
      return { action: 'reconnect', title: t('session.offlineTitle'), text: t(view === 'terminal' ? 'terminal.offline' : view === 'files' ? 'files.offline' : 'session.offlineText') };
    case 'ended':
      // A server bye after establishment means the restarted computer no longer names the session.
      if (via === 'bye' && established) return { action: 'reconnect', title: t('session.restartedTitle'), text: endedText('session.restartedText') };
      return { title: t('session.endedTitle'), text: endedText('session.endedText') };
    case 'busy': return takeoverMessage(controller, t);
    case 'taken':
      return {
        action: 'takeBack',
        title: same ? t('session.takenHereTitle') : t('session.takenTitle', { name: by || t('session.someone') }),
        text: t(view === 'desktop' ? 'session.takenText' : 'session.dataTakenText', { time: time() }),
      };
    case 'unpaired':
      if (!headless) return { action: 'pair' };
      return {
        action: 'devices',
        title: t('notAdded.untrustedTitle'),
        text: t('notAdded.browserApproval', { devices: t('devices.title'), current: t('devices.tagThis'), signOut: t('devices.signOut') }),
      };
    case 'auth':
      return { action: headless ? 'devices' : 'addAgain', title: t('session.authTitle'), text: t('session.authText') };
    case 'locked':
    case 'no_user_session': return { action: 'desktop', title: t('session.endedTitle'), text: workspaceNotice(reason, t) };
    case 'user_changed': return { title: t('session.endedTitle'), text: t('detail.workspaceUserChanged') };
    case 'view_only': return { title: workspaceNotice(reason, t) };
    case 'no_terminal': return { title: t('terminal.unavailable') };
    case 'permissions': return { title: t('session.permissionsTitle'), text: t('session.permissionsText') };
  }
}
