// What the AI activity on a computer's page asks for (views/audit.js), from what the person
// chose: a time range, an AI client, a tool, failures only, and the page. Kept apart from the
// view so it is plain to test.
//
// Public surface: PER_PAGE, RANGES, auditQuery(choice, now).

export const PER_PAGE = 50;
export const RANGES = ['all', 'today', 'week', 'month', 'custom'];

const DAY = 24 * 60 * 60;

// The start of the day `date` is in, local time, as unix seconds.
const dayStart = (date) => Math.floor(new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime() / 1000);

// `choice`: { range, from, to ('YYYY-MM-DD', custom only), client, tool, failed, page }.
export function auditQuery(choice, now = new Date()) {
  const q = { limit: PER_PAGE, offset: (choice.page || 0) * PER_PAGE };
  const today = dayStart(now);
  if (choice.range === 'today') q.since = today;
  if (choice.range === 'week') q.since = today - 6 * DAY;
  if (choice.range === 'month') q.since = today - 29 * DAY;
  if (choice.range === 'custom') {
    const day = (s) => (s ? dayStart(new Date(`${s}T00:00:00`)) : null);
    const [from, to] = [day(choice.from), day(choice.to)];
    if (from != null) q.since = from;
    if (to != null) q.until = to + DAY - 1;
  }
  if (choice.client) q.client = choice.client;
  if (choice.tool) q.tool = choice.tool;
  if (choice.failed) q.failed = true;
  return q;
}
