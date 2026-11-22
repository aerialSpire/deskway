// How this browser wants the remote screen: 'sharp' (the default: at most a couple of frames a
// second, each one sharp, for text and work) or 'smooth' (up to the host's full rate, softer
// while the screen moves). Kept per browser, like the other settings of this device. The choice
// reaches the host as the frame rate of the `view` message (session/constraints.js).
import { store } from './env.js';

export const PICTURES = Object.freeze(['sharp', 'smooth']);

export const picture = () => (store.get('deskway.picture') === 'smooth' ? 'smooth' : 'sharp');

export const setPicture = (p) => store.set('deskway.picture', p);
