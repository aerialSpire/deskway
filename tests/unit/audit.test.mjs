// What the AI activity on a computer's page asks the computer for (web/static/js/views/audit-query.js).
//
//   node --test tests/unit/audit.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { PER_PAGE, auditQuery } from '../../web/static/js/views/audit-query.js';

const DAY = 86400;

test('each choice asks for its calls, a page at a time', () => {
  const now = new Date(2026, 10, 20, 15, 30);
  const today = new Date(2026, 10, 20).getTime() / 1000;
  const base = { range: 'all', from: '', to: '', client: '', tool: '', failed: false, page: 0 };
  const cases = [
    [{}, { limit: PER_PAGE, offset: 0 }],
    [{ page: 2 }, { limit: PER_PAGE, offset: 2 * PER_PAGE }],
    [{ range: 'today' }, { limit: PER_PAGE, offset: 0, since: today }],
    [{ range: 'week' }, { limit: PER_PAGE, offset: 0, since: today - 6 * DAY }],
    [{ range: 'month' }, { limit: PER_PAGE, offset: 0, since: today - 29 * DAY }],
    [{ range: 'custom', from: '2026-11-01', to: '2026-11-02' }, { limit: PER_PAGE, offset: 0, since: new Date(2026, 10, 1).getTime() / 1000, until: new Date(2026, 10, 3).getTime() / 1000 - 1 }],
    [{ range: 'custom', from: '', to: '' }, { limit: PER_PAGE, offset: 0 }],
    [{ client: 'Claude', tool: 'run', failed: true }, { limit: PER_PAGE, offset: 0, client: 'Claude', tool: 'run', failed: true }],
  ];
  for (const [choice, want] of cases) assert.deepEqual(auditQuery({ ...base, ...choice }, now), want, JSON.stringify(choice));
});
