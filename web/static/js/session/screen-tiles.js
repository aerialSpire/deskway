// Reliable, original-size PNG updates on the channel negotiated by the host's capability.
const SIDE = 64;
const HEADER = 20;
const PNG_HEADER = HEADER + 24;

export function readTile(data) {
  if (!(data instanceof ArrayBuffer) || data.byteLength < PNG_HEADER || data.byteLength > 16 * 1024) return null;
  const view = new DataView(data);
  const revision = view.getBigUint64(4, true);
  const w = view.getUint16(12, true);
  const h = view.getUint16(14, true);
  const x = view.getUint16(16, true);
  const y = view.getUint16(18, true);
  if (!w || !h || w > 8192 || h > 8192 || w * h > 8192 * 4320 || x >= w || y >= h || x % SIDE || y % SIDE) return null;
  const width = Math.min(SIDE, w - x);
  const height = Math.min(SIDE, h - y);
  if (view.getUint32(HEADER) !== 0x89504e47 || view.getUint32(HEADER + 12) !== 0x49484452 ||
      view.getUint32(HEADER + 16) !== width || view.getUint32(HEADER + 20) !== height) return null;
  return { revision, w, h, x, y, width, height };
}

export class ScreenTiles {
  constructor(canvas, video, painted) {
    this.canvas = canvas;
    this.video = video;
    this.painted = painted;
    this.ctx = canvas.getContext('2d', { alpha: false });
    this.channel = null;
    this.wanted = false;
    this.serial = Promise.resolve();
    this.token = 0;
    this.revision = null;
    this.unavailableRevision = null;
    this.overlayVideo = true;
  }

  get active() { return !this.canvas.hidden; }

  get supported() { return !!this.ctx && typeof createImageBitmap === 'function'; }

  attach(ch) {
    if (!this.supported || this.channel) {
      ch.close();
      return;
    }
    this.channel = ch;
    ch.binaryType = 'arraybuffer';
    const ready = () => this.send(this.wanted ? 'allow' : 'video');
    ch.onopen = ready;
    ch.onclose = () => this.clear();
    ch.onmessage = ({ data }) => {
      // Decode in wire order; an old completion must not paint after an exit or reset.
      this.serial = this.serial.then(() => this.receive(data)).catch(() => this.fallback());
    };
    if (ch.readyState === 'open') ready();
  }

  update(wanted) {
    if (wanted === this.wanted) return;
    this.wanted = wanted;
    this.send(wanted ? 'allow' : 'video');
    if (!wanted) this.clear();
  }

  send(data) {
    const ch = this.channel;
    if (ch?.readyState === 'open') ch.send(data);
  }

  async receive(data) {
    if (data === 'video') {
      this.clear();
      return;
    }
    const tile = readTile(data);
    if (!tile) throw new Error('Invalid screen tile');
    if (this.wanted && tile.revision !== this.unavailableRevision) {
      const token = this.token;
      const image = await createImageBitmap(new Blob([data.slice(HEADER)], { type: 'image/png' }));
      try {
        if (token === this.token && this.wanted) {
          if (image.width !== tile.width || image.height !== tile.height) throw new Error('Invalid tile size');
          const newScreen = this.revision !== tile.revision;
          if (newScreen) {
            this.canvas.width = tile.w;
            this.canvas.height = tile.h;
            this.revision = tile.revision;
            // The last video picture fills the initially unpainted area for this display.
            if (this.overlayVideo && this.video.videoWidth && this.video.readyState >= 2 &&
                Math.abs(this.video.videoWidth / this.video.videoHeight - tile.w / tile.h) < 0.02) {
              this.ctx.drawImage(this.video, 0, 0, tile.w, tile.h);
            }
            this.overlayVideo = false;
          }
          this.ctx.drawImage(image, tile.x, tile.y);
          this.canvas.hidden = false;
          this.painted(newScreen);
        }
      } finally {
        image.close();
      }
    }
    // The host budgets outstanding bytes until the corresponding tile was applied.
    this.send(data.slice(0, 4));
  }

  fallback() {
    this.clear();
    // Close instead of reopening a failing channel in the middle of the same session.
    this.channel?.close();
  }

  clear(useVideo = true) {
    this.token++;
    this.unavailableRevision = useVideo ? null : this.revision;
    this.revision = null;
    this.overlayVideo = useVideo;
    this.canvas.hidden = true;
    this.canvas.width = 0;
    this.canvas.height = 0;
  }
}
