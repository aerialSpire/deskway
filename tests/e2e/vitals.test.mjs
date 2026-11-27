// The live computer's status and usage layouts on the overview and detail pages.
import { sleep } from './harness.mjs';

export const area = {
  name: 'vitals',
  needs: ['auth'],
  async run(h) {
    const a = h.a;
    await a.until(`!!document.querySelector('.card .vitals .gauge')`, 'the computer reports vitals');
    // The first sample can precede the first keepalive pong. Reopen the overview after
    // the existing preview expires to see the next sample.
    if (!await a.eval(`!!document.querySelector('.card .lat')`)) {
      await sleep(31000);
      await a.eval(`document.querySelector('.card-open').click(); document.querySelector('.title-back').click()`);
      await a.until(`!!document.querySelector('.card .lat')`, 'the computer reports its measured round trip');
    }
    const rightAligned = (where) => a.eval(`(() => {
      const status = document.querySelector(${JSON.stringify(where)} + ' .computer-status .status');
      const version = status.querySelector('.ver');
      return version && Math.abs(version.getBoundingClientRect().right - status.getBoundingClientRect().right) < 1;
    })()`);
    for (const width of [1280, 390]) {
      await a.shot(`vitals-list-${width}`, width, 900);
      h.check(`list at ${width}px: version uses the right edge of the status line`, await rightAligned('.card'));
      h.check(`list at ${width}px: metrics use one row across the card`, await a.eval(`
        new Set([...document.querySelectorAll('.card .gauge')].map((gauge) => gauge.offsetTop)).size === 1`));
    }

    await a.eval(`document.querySelector('.card-open').click()`);
    await a.until(`!!document.querySelector('.vitals-card')`, 'the computer page shows its vitals card');
    for (const [width, columns] of [[1280, 3], [390, 2]]) {
      await a.shot(`vitals-detail-${width}`, width, 1050);
      h.check(`detail at ${width}px: version uses the right edge of the status line`, await rightAligned('.heading'));
      h.check(`detail at ${width}px: gauges reflow below actions and above AI activity`, await a.eval(`(() => {
        const card = document.querySelector('.vitals-card');
        const tracks = getComputedStyle(card.querySelector('.vitals')).gridTemplateColumns.split(' ').filter((track) => parseFloat(track) > 0).length;
        const gauges = [...card.querySelectorAll('.gauge')];
        const rows = new Set(gauges.map((gauge) => gauge.offsetTop)).size;
        const firstRowBars = gauges.slice(0, tracks).map((gauge) => gauge.querySelector('.g-track').getBoundingClientRect().top);
        return (${width === 1280 ? `tracks >= ${columns} && rows === 1` : `tracks === ${columns}`})
          && firstRowBars.every((y) => y === firstRowBars[0])
          && card.previousElementSibling?.matches('.computer-actions')
          && card.nextElementSibling?.matches('.audit');
      })()`));
    }
    await a.eval(`document.querySelector('.title-back').click()`);
  },
};
