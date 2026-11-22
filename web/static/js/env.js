// What this browser and device are, and the small things every module needs from them.

// Web storage, which private modes and blocked storage turn into a no-op. Reaching the storage
// object itself can throw too, so it is looked up on every use.
const kept = (storage) => ({
  get(k) { try { return storage().getItem(k); } catch { return null; } },
  set(k, v) { try { storage().setItem(k, v); } catch { /* private mode */ } },
  remove(k) { try { storage().removeItem(k); } catch { /* private mode */ } },
});
// Kept in this browser until cleared.
export const store = kept(() => localStorage);
// Kept in this tab only, until it closes.
export const sessionStore = kept(() => sessionStorage);

export const buzz = (ms) => { if (navigator.vibrate) navigator.vibrate(ms); };

export const randomId = () => Array.from(crypto.getRandomValues(new Uint8Array(12)), (b) => b.toString(16).padStart(2, '0')).join('');

// Phones and tablets: the screen works as a trackpad or by tapping where to click.
export const COARSE = matchMedia('(pointer: coarse)').matches;
// A precise pointer (mouse, touchpad) is attached; Pointer Lock only means something with one.
export const HAS_MOUSE = () => matchMedia('(any-pointer: fine)').matches;
// A touch screen anywhere, also on a laptop driven mostly by its touchpad.
export const HAS_TOUCH = COARSE || matchMedia('(any-pointer: coarse)').matches || navigator.maxTouchPoints > 0;
// This device is a Mac (not an iPad, which reports itself as one).
export const CONTROLLER_MAC = /Mac/.test((navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || '') && navigator.maxTouchPoints < 2;
// Floating panels remember their place per orientation.
export const orientation = () => (matchMedia('(orientation: portrait)').matches ? 'portrait' : 'landscape');

// Opened as an installed app (any browser), not in a tab.
export const standalone = () => matchMedia('(display-mode: standalone)').matches || navigator.standalone === true;
// Opened from the Home Screen in Safari: that app keeps its own storage, apart from Safari's, so
// it signs in and is added to the computers as a device of its own.
export const separateHomeScreenApp = () => navigator.standalone === true;
