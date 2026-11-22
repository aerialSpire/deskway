// The computer this browser runs on (docs/session-lifecycle.md, "The computer you are using"):
// the page says where it runs, and the computer of the account that is that place says so; the
// device list then marks it `here`. A browser cannot see its own addresses, but WebRTC names
// them for it (mDNS host names, name.local), and only the computer they resolve to its own
// addresses on is this one. Only a hint: pressing that computer asks first (views/takeover.js).
//
// Public surface: locate(), locating(on).
import { socket } from './net.js';
import { Signal } from './proto.js';

// The names, once gathered. They exist while the peer connection that made them does, so it
// stays open with the page.
let names = null;
let pc = null;
let on = true;

// This browser's mDNS host names: a peer connection with no servers gathers only those.
async function gather() {
  if (names) return names;
  const found = new Set();
  try {
    pc = new RTCPeerConnection();
    pc.createDataChannel('here');
    const done = new Promise((resolve) => {
      pc.onicecandidate = (e) => {
        if (!e.candidate) { resolve(); return; }
        const address = e.candidate.address || e.candidate.candidate.split(' ')[4];
        if (address && address.endsWith('.local')) found.add(address);
      };
      setTimeout(resolve, 3000);
    });
    await pc.setLocalDescription(await pc.createOffer());
    await done;
  } catch {
    // No WebRTC here: nothing to say.
  }
  names = [...found].slice(0, 8);
  return names;
}

// Says where this browser runs, on every new socket: the server forgets it with the old one.
export async function locate() {
  if (!on) return;
  const where = await gather();
  if (on && where.length) socket.sendNow({ type: Signal.LOCATE, where });
}

// Whether this page says where it runs (it does unless told otherwise: the e2e tests, whose
// browser runs on the computer they test); off says it runs nowhere known.
export function locating(yes) {
  on = yes;
  if (yes) locate();
  else socket.sendNow({ type: Signal.LOCATE, where: [] });
}
