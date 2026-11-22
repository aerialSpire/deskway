// The toolbar's connection details: one list of readings, with the previous window when
// available. The same panel stays mounted while values change, so focus is preserved.
import { h } from '../dom.js';
import { currentLang, t } from '../i18n.js';
import { SHARP_FPS } from './constraints.js';

const METRICS = [
  ['rtt', (n, f) => `${f(n, 0)} ms`],
  ['loss', (n, f) => `${f(n * 100, 1, 1)}%`],
  ['fps', (n, f) => `${f(n, 1)} fps`],
  ['bitrate', (n, f) => n < 1_000_000 ? `${f(n / 1_000, 0)} kbps` : `${f(n / 1_000_000, 1)} Mbps`],
];

export class ConnectionDetails {
  constructor(close) {
    const id = 'connection-details';
    const heading = h('h2', { id: `${id}-title` }, t('connectionDetails.title'));
    const dismiss = h('button', { type: 'button', class: 'btn compact icon', 'aria-label': t('common.close'), onclick: close }, '×');
    this.previous = h('th', { scope: 'col' }, t('connectionDetails.previous'));
    this.header = h('thead', {}, h('tr', {}, h('th', { scope: 'col', 'aria-label': t('connectionDetails.metric') }),
      h('th', { scope: 'col' }, t('connectionDetails.recent')), this.previous));
    this.sharp = h('span', { class: 'reading-note', hidden: true }, t('connectionDetails.sharp', { fps: SHARP_FPS }));
    this.rows = METRICS.map(([key, format]) => {
      const current = h('td');
      const previous = h('td');
      const label = h('th', { scope: 'row' }, t(`connectionDetails.${key}`), key === 'fps' ? this.sharp : null);
      return { key, format, current, previous, el: h('tr', {}, label, current, previous) };
    });
    const readings = h('table', { class: 'connection-readings' }, this.header, h('tbody', {}, this.rows.map((r) => r.el)));
    this.el = h('section', { id, class: 'panel connection-details', 'aria-labelledby': heading.id },
      h('div', { class: 'connection-details-head' }, heading, dismiss), readings);
  }

  update({ current, previous, mode }) {
    const hasVideo = this.rows.some(({ key }) => key !== 'rtt' && current[key] != null);
    const hasPrevious = this.rows.some(({ key }) => current[key] != null && previous[key] != null);
    this.header.hidden = !hasPrevious;
    this.previous.hidden = !hasPrevious;
    this.sharp.hidden = mode !== 'sharp';
    const number = (value, maximumFractionDigits, minimumFractionDigits = 0) =>
      value.toLocaleString(currentLang(), { maximumFractionDigits, minimumFractionDigits });
    for (const row of this.rows) {
      const { key, format } = row;
      row.el.hidden = key !== 'rtt' && !hasVideo;
      row.previous.hidden = !hasPrevious;
      row.current.textContent = current[key] == null ? '—' : format(current[key], number);
      row.previous.textContent = current[key] == null || previous[key] == null ? '—' : format(previous[key], number);
    }
    return hasVideo || current.rtt != null;
  }
}
