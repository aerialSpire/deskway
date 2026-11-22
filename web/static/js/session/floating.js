// What floats over the remote screen and can be moved (the on-screen keyboard, the mouse
// panel): dragging it by its handle, and where it was put, remembered per orientation.
//
// Public surface: draggable(handle, { move, start, end, axis, slop, ignore }), savedPlace(name),
// savePlace(name, place).
import { store, orientation } from '../env.js';

// Dragging `handle`. `start(e)` when a pointer goes down on it; `move(dx, dy)` with the pointer's
// travel since then, once it is more than `slop` pixels; `end(moved, cancelled)` when it lifts.
// `axis` 'y' counts vertical travel only (dx is then 0). `ignore(e)` leaves a press alone (a
// button inside the handle). The handle's pointer events stay its own: the screen under it never
// sees them.
export function draggable(handle, { move, start = () => {}, end = () => {}, axis = 'xy', slop = 0, ignore = () => false }) {
  let drag = null;
  const travel = (e) => ({ dx: axis === 'y' ? 0 : e.clientX - drag.x, dy: e.clientY - drag.y });
  handle.addEventListener('pointerdown', (e) => {
    if (ignore(e)) return;
    e.stopPropagation();
    e.preventDefault();
    drag = { x: e.clientX, y: e.clientY, moved: false };
    try { handle.setPointerCapture(e.pointerId); } catch { /* the pointer is already gone */ }
    start(e);
  });
  handle.addEventListener('pointermove', (e) => {
    if (!drag) return;
    e.stopPropagation();
    const { dx, dy } = travel(e);
    if (!drag.moved && Math.hypot(dx, dy) <= slop) return;
    drag.moved = true;
    move(dx, dy);
  });
  const lift = (e, cancelled) => {
    if (!drag) return;
    e.stopPropagation();
    const { moved } = drag;
    drag = null;
    end(moved, cancelled);
  };
  handle.addEventListener('pointerup', (e) => lift(e, false));
  handle.addEventListener('pointercancel', (e) => lift(e, true));
}

const placeKey = (name) => `deskway.${name}.${orientation()}`;

// Where `name` was last put in this orientation ({ x, y }, fractions of the room it has), or null.
export function savedPlace(name) {
  try { return JSON.parse(store.get(placeKey(name)) || 'null'); } catch { return null; }
}

export const savePlace = (name, place) => store.set(placeKey(name), JSON.stringify(place));
