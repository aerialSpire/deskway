import assert from 'node:assert/strict';
import { test } from 'node:test';
import { offerMessage } from '../../web/static/js/views/offer-message.js';

const t = (key) => key;
const time = () => '12:00';
const auth = { title: 'session.authTitle', text: 'session.authText' };
const busy = { action: 'takeover', title: 'force.account', go: 'force.accountGo' };
const taken = { action: 'takeBack', title: 'session.takenTitle' };
const unavailable = { title: 'terminal.unavailable' };
const permissions = { title: 'session.permissionsTitle', text: 'session.permissionsText' };
const failed = { action: 'reconnect', title: 'session.failedTitle' };
const offline = { action: 'reconnect', title: 'session.offlineTitle' };
const ended = { title: 'session.endedTitle' };
const restarted = { action: 'reconnect', title: 'session.restartedTitle' };

for (const [reason, details, desktop, terminal] of [
  ['unpaired', {}, { action: 'pair' }, { action: 'devices', title: 'notAdded.untrustedTitle', text: 'notAdded.browserApproval' }],
  ['auth', {}, { ...auth, action: 'addAgain' }, { ...auth, action: 'devices' }],
  ['busy', { controller: { name: 'Phone' } }, { ...busy, text: 'force.accountText' }, { ...busy, text: 'terminal.takeoverText' }],
  ['taken', { by: 'Phone' }, { ...taken, text: 'session.takenText' }, { ...taken, text: 'terminal.taken' }],
  ['failed', { established: true }, { ...failed, text: 'session.failedText' }, { ...failed, text: 'terminal.ended' }],
  ['gone', { established: true, cause: 'host' }, { ...offline, text: 'session.offlineText' }, { ...offline, text: 'terminal.ended' }],
  ['ended', { established: true, via: 'end' }, { ...ended, text: 'session.endedText' }, { ...ended, text: 'terminal.ended' }],
  ['ended', { established: true, via: 'bye' }, { ...restarted, text: 'session.restartedText' }, { ...restarted, text: 'terminal.ended' }],
  ['no_terminal', {}, unavailable, unavailable],
  ['permissions', {}, permissions, permissions],
]) {
  for (const [kind, expected] of [['desktop', desktop], ['terminal', terminal]]) {
    test(`${kind} shows the message and action for ${reason}${details.via ? ' via ' + details.via : ''}`, () => {
      const onTerminal = kind === 'terminal';
      assert.deepEqual(offerMessage({ reason, ...details }, { terminal: onTerminal, headless: onTerminal, t, time }), expected);
    });
  }
}
