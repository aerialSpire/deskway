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

const needsDevice = { action: 'devices', title: 'notAdded.untrustedTitle', text: 'notAdded.browserApproval' };
const takeover = { ...busy, text: 'force.accountText' };
const takenData = { ...taken, text: 'session.dataTakenText' };

for (const [reason, details, desktop, terminal, files = terminal] of [
  ['unpaired', {}, { action: 'pair' }, needsDevice],
  ['auth', {}, { ...auth, action: 'addAgain' }, { ...auth, action: 'devices' }],
  ['busy', { controller: { name: 'Phone' } }, takeover, takeover],
  ['taken', { by: 'Phone' }, { ...taken, text: 'session.takenText' }, takenData],
  ['failed', { established: true }, { ...failed, text: 'session.failedText' }, { ...failed, text: 'terminal.ended' }, { ...failed, text: 'files.ended' }],
  ['gone', { established: true, cause: 'host' }, { ...offline, text: 'session.offlineText' }, { ...offline, text: 'terminal.ended' }, { ...offline, text: 'files.ended' }],
  ['ended', { established: true, via: 'end' }, { ...ended, text: 'session.endedText' }, { ...ended, text: 'terminal.ended' }, { ...ended, text: 'files.ended' }],
  ['ended', { established: true, via: 'bye' }, { ...restarted, text: 'session.restartedText' }, { ...restarted, text: 'terminal.ended' }, { ...restarted, text: 'files.ended' }],
  ['no_terminal', {}, unavailable, unavailable],
  ['permissions', {}, permissions, permissions],
]) {
  for (const [view, expected] of [['desktop', desktop], ['terminal', terminal], ['files', files]]) {
    test(`${view} shows the message and action for ${reason}${details.via ? ' via ' + details.via : ''}`, () => {
      assert.deepEqual(offerMessage({ reason, ...details }, { view, headless: view !== 'desktop', t, time }), expected);
    });
  }
}
