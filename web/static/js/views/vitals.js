// Components shared by the computer list and detail page.
import { h } from '../dom.js';
import { t } from '../i18n.js';

export function latency(d) {
  const rtt = d.online ? d.vitals?.rtt : undefined;
  if (!Number.isFinite(rtt)) return null;
  const bars = rtt < 30 ? 4 : rtt < 80 ? 3 : rtt < 150 ? 2 : 1;
  return h('span', { class: bars === 1 ? 'lat warn' : 'lat', title: t('vitals.latency') },
    h('span', { class: 'signal', 'aria-hidden': 'true' },
      Array.from({ length: 4 }, (_, i) => h('i', { class: i < bars ? 'on' : '' }))),
    t(rtt ? 'vitals.ms' : 'vitals.subMs', { rtt }));
}

function gauge(label, usage, sub) {
  if (!Number.isFinite(usage)) return null;
  const percent = Math.round(usage);
  const level = usage >= 95 ? ' high' : usage >= 80 ? ' warn' : '';
  return h('div', { class: `gauge${level}` },
    h('span', { class: 'g-label' }, label),
    h('span', { class: 'g-value' }, `${percent}%`),
    h('span', { class: 'g-track', 'aria-hidden': 'true' },
      h('span', { class: 'g-fill', style: `width: ${percent}%` })),
    sub ? h('span', { class: 'g-sub' }, sub) : null);
}

const usedPercent = (usage) => usage?.total > 0 ? 100 * usage.used / usage.total : undefined;

function capacity({ used, total }) {
  const unit = total >= 1e12 ? 1e12 : 1e9;
  const size = (bytes) => {
    const value = bytes / unit;
    return `${value.toFixed(value < 10 ? 1 : 0)} ${unit === 1e12 ? 'TB' : 'GB'}`;
  };
  return t('vitals.capacity', { used: size(used), total: size(total) });
}

function cpuModel(machine) {
  if (!machine) return '';
  const { cpu, cores, threads } = machine;
  const topology = [
    cores > 0 && t('vitals.cores', { n: cores }),
    threads > 0 && t('vitals.threads', { n: threads }),
  ].filter(Boolean).join(' ');
  return [cpu, topology].filter(Boolean).join(' · ');
}

export function vitalsBars(d, { detail = false } = {}) {
  if (!d.online || !d.vitals) return null;
  const { cpu, memory, disks } = d.vitals;
  const systemDisk = d.os === 'windows' ? 'C:' : '/';
  const shownDisks = detail ? disks : disks.filter((disk) => disk.name === systemDisk);
  const gauges = [
    gauge(t('vitals.cpu'), cpu, detail ? cpuModel(d.machine) : ''),
    gauge(t(detail ? 'vitals.memory' : 'vitals.memoryShort'), usedPercent(memory), detail ? capacity(memory) : ''),
    ...shownDisks.map((disk) => gauge(
      detail ? t('vitals.diskName', { name: disk.name }) : t('vitals.disk'),
      usedPercent(disk), detail ? capacity(disk) : '')),
  ].filter(Boolean);
  if (!gauges.length) return null;
  const bars = h('div', { class: detail ? 'vitals large' : 'vitals' }, gauges);
  return detail ? h('section', { class: 'vitals-card', 'aria-label': t('vitals.title') }, bars) : bars;
}
