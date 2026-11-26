// WebRTC readings for the current ICE path. The sampler follows the session; statistics never
// decide whether it is connected. Windows use counters from the same inbound video stream.
const WINDOW_MS = 10_000;
const SAMPLE_MS = 1_000;

function selectedPair(report, transportId) {
  const selected = report.get(report.get(transportId)?.selectedCandidatePairId);
  if (selected) return selected;
  let nominated = null;
  for (const stat of report.values()) {
    if (stat.type === 'transport' && stat.selectedCandidatePairId) {
      const pair = report.get(stat.selectedCandidatePairId);
      if (pair) return pair;
    }
    if (stat.type === 'candidate-pair' && stat.nominated && stat.state === 'succeeded') nominated = stat;
  }
  return nominated;
}

export function describePath(pair, local, remote) {
  if (!pair) return { route: 'connected' };
  const localType = local?.candidateType;
  const remoteType = remote?.candidateType;
  const browser = localType === 'relay' ? local : null;
  const computer = remoteType === 'relay' ? remote : null;
  if (browser || computer) {
    const browserRelay = browser?.url?.match(/^turns?:([^?]+)/)?.[1];
    const relayProtocol = browser?.relayProtocol;
    const computerRelay = computer?.address;
    return {
      route: 'relay',
      ...(computerRelay && { computerRelay }),
      ...(browserRelay && { browserRelay }),
      ...(relayProtocol && { relayProtocol }),
    };
  }
  if (!localType || !remoteType) return { route: 'connected' };
  return { route: 'direct', sameNetwork: localType === 'host' && remoteType === 'host' };
}

const valid = (value) => Number.isFinite(value) && value >= 0;
const delta = (first, last) => valid(first) && valid(last) && last >= first ? last - first : null;

function median(values) {
  if (!values.length) return null;
  const ordered = values.sort((a, b) => a - b);
  const middle = Math.floor(ordered.length / 2);
  return (ordered[middle] + ordered[Math.floor((ordered.length - 1) / 2)]) / 2;
}

function readings(samples, generation) {
  const rtt = median(samples.map((s) => s.rtt).filter(valid));
  const video = samples.filter((s) => s.video && s.generation === generation);
  if (video.length < 2) return { rtt };
  const a = video[0];
  const b = video.at(-1);
  const seconds = (b.video.timestamp - a.video.timestamp) / 1000;
  if (!Number.isFinite(seconds) || seconds <= 0) return { rtt };
  const received = delta(a.video.packetsReceived, b.video.packetsReceived);
  // Late packets can bring the cumulative lost counter down, including below zero.
  const lost = Number.isFinite(a.video.packetsLost) && Number.isFinite(b.video.packetsLost)
    ? Math.max(0, b.video.packetsLost - a.video.packetsLost) : null;
  const frames = delta(a.video.framesDecoded, b.video.framesDecoded);
  const bytes = delta(a.video.bytesReceived, b.video.bytesReceived);
  return {
    rtt,
    loss: received !== null && lost !== null && received + lost > 0 ? lost / (received + lost) : null,
    fps: frames === null ? null : frames / seconds,
    bitrate: bytes === null ? null : bytes * 8 / seconds,
  };
}

export class ConnectionHistory {
  constructor() {
    this.samples = [];
    this.pairId = null;
    this.generation = 0;
  }

  update(report, now, mode = null) {
    const streams = [...report.values()].filter((s) => s.type === 'inbound-rtp' && s.kind === 'video');
    const incoming = streams.reduce((best, s) => !best || (s.lastPacketReceivedTimestamp ?? 0) > (best.lastPacketReceivedTimestamp ?? 0) ? s : best, null);
    const pair = selectedPair(report, incoming?.transportId);
    const path = describePath(pair, report.get(pair?.localCandidateId), report.get(pair?.remoteCandidateId));
    if (!pair) {
      this.samples = [];
      this.pairId = null;
      return { path, current: {}, previous: {}, mode };
    }
    const latest = this.samples.at(-1);
    if (pair.id !== this.pairId || (latest && now - latest.at > WINDOW_MS)) this.samples = [];
    this.pairId = pair.id;

    const video = mode && incoming?.framesDecoded > 0 ? incoming : null;
    const last = this.samples.at(-1);
    if (last?.video?.id !== video?.id || last?.mode !== mode || video?.framesDecoded < last?.video?.framesDecoded) {
      this.generation++;
    }
    this.samples.push({ at: now, rtt: valid(pair.currentRoundTripTime) ? pair.currentRoundTripTime * 1000 : null, video, mode, generation: this.generation });

    const split = now - WINDOW_MS;
    const start = split - WINDOW_MS;
    // Retain one older sample to tell when a complete comparison window is available.
    while (this.samples.length > 1 && this.samples[1].at <= start) this.samples.shift();
    const generation = video ? this.generation : null;
    const current = readings(this.samples.filter((s) => s.at >= split), generation);
    const oldest = this.samples[0];
    const previous = oldest.at <= start
      ? readings(this.samples.filter((s) => s.at >= start && s.at <= split), oldest.generation === generation ? generation : null)
      : {};
    return { path, current, previous, mode };
  }
}

export class ConnectionStats {
  constructor(getMode, publish) {
    this.getMode = getMode;
    this.publish = publish;
    this.watch = null;
  }

  start(pc) {
    if (!pc || pc === this.watch?.pc) return;
    this.stop();
    const watch = { pc, history: new ConnectionHistory(), timer: null };
    this.watch = watch;
    this.sample(watch);
  }

  async sample(watch) {
    let report;
    try { report = await watch.pc.getStats(); } catch { report = new Map(); }
    // An outstanding getStats call may finish after a disconnect or ICE recovery.
    if (watch !== this.watch) return;
    this.publish(watch.history.update(report, performance.now(), this.getMode()));
    if (watch === this.watch) watch.timer = setTimeout(() => this.sample(watch), SAMPLE_MS);
  }

  stop() {
    clearTimeout(this.watch?.timer);
    this.watch = null;
  }
}
