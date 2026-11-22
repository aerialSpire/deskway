// What this browser shows and decodes, as the `view` message the host sizes its video by.
// Pure: no page state, so tests/unit/constraints.test.mjs runs it with a stubbed
// MediaCapabilities.
//
// Public surface: FPS, LADDER, rungConfig(rung), decodeLimit(mediaCapabilities),
// viewport(rect, dpr, zoom), watchPixelRatio(window, changed), viewMessage(viewport, limit, sharp),
// SHARP_FPS.
import { Control } from '../proto.js';

// The host's most frames a second; probing faster would promise what is never sent.
export const FPS = 24;
// The sharp picture's frames a second: the host gives each one far more bits (video::size).
export const SHARP_FPS = 2;

// Landscape sizes to probe, smallest first, each with the lowest H.264 level (Table A-1) that
// holds it at FPS. The host's tests (video::size) check the same pairs.
export const LADDER = Object.freeze([
  [1280, 720, 0x1f],
  [1920, 1080, 0x28],
  [2560, 1440, 0x32],
  [3840, 2160, 0x33],
  [5120, 2880, 0x3c],
  [7680, 4320, 0x3c],
]);

const hex2 = (n) => n.toString(16).padStart(2, '0');

// The question for one rung: can this browser receive H.264 High at this size and level over
// WebRTC? The bit rate is the Kush gauge at low motion, as the host's ceiling.
export function rungConfig([width, height, level]) {
  return {
    type: 'webrtc',
    video: {
      contentType: `video/H264;profile-level-id=6400${hex2(level)};packetization-mode=1`,
      width,
      height,
      bitrate: Math.round(width * height * FPS * 0.07),
      framerate: FPS,
    },
  };
}

// The largest rung reported supported and smooth, as { w, h, fps }; null when the API is
// missing or nothing is. The ladder only grows, so the first refusal ends it.
export async function decodeLimit(mc) {
  if (!mc || typeof mc.decodingInfo !== 'function') return null;
  let best = null;
  for (const rung of LADDER) {
    let info;
    try { info = await mc.decodingInfo(rungConfig(rung)); } catch { break; }
    if (!info || !info.supported || !info.smooth) break;
    best = { w: rung[0], h: rung[1], fps: FPS };
  }
  return best;
}

// The stage in device pixels, times the zoom: what the picture is shown at. A stage with no
// area (a page being hidden or laid out again) shows nothing: there is no viewport, and nothing
// is reported for it.
export function viewport(rect, dpr = 1, zoom = 1) {
  const k = (dpr || 1) * (zoom || 1);
  const w = Math.round(rect.width * k);
  const h = Math.round(rect.height * k);
  return w > 0 && h > 0 ? { w, h } : null;
}

// Calls `changed` whenever the device pixel ratio changes (the window moved to another
// monitor), which resizes nothing. A media query for the current ratio stops matching once; it
// is then armed again for the new ratio. Returns the function that stops watching.
export function watchPixelRatio(win, changed) {
  let stop = () => {};
  const arm = () => {
    const mq = win.matchMedia(`(resolution: ${win.devicePixelRatio}dppx)`);
    const fire = () => { arm(); changed(); };
    mq.addEventListener('change', fire, { once: true });
    stop = () => mq.removeEventListener('change', fire);
  };
  arm();
  return () => stop();
}

// The message. Without a decode limit, the viewport stands in for it. `sharp`: the person prefers
// a sharper picture to a smoother one, so the host is asked for SHARP_FPS frames a second.
export function viewMessage(vp, limit, sharp = false) {
  const max = limit || vp;
  const smooth = limit ? limit.fps : FPS;
  return { t: Control.VIEW, w: vp.w, h: vp.h, maxW: max.w, maxH: max.h, fps: sharp ? Math.min(SHARP_FPS, smooth) : smooth };
}
