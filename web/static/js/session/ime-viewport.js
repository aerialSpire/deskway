// The viewport is in layout CSS pixels. Compare unzoomed heights so browser zoom alone
// cannot look like a keyboard opening.
export function keyboardArea(view, pageHeight, before, keyboard) {
  const height = view.h * view.scale;
  const fromLayout = pageHeight - height;
  const fromBefore = Math.abs(view.w * view.scale - before.w * before.scale) <= 2
    ? (before.h * before.scale) - height : 0;
  const threshold = Math.min(100, Math.max(48, pageHeight * 0.15));
  if (!keyboard?.height && Math.max(fromLayout, fromBefore) <= threshold) return null;

  // A reported overlay can also cover an unchanged visual viewport. Use the room above
  // it, including for a floating keyboard, and intersect with the reported visible area.
  const overlaps = keyboard?.height > 0 && keyboard.left < view.x + view.w && keyboard.right > view.x;
  const h = overlaps ? Math.max(0, Math.min(view.h, keyboard.top - view.y)) : view.h;
  return { x: view.x, y: view.y, w: view.w, h };
}

// Use the height left above the strip, up to the height of the original picture viewport.
export function pictureArea(view, stripTop, originalHeight) {
  const h = Math.max(0, Math.min(originalHeight, stripTop - view.y));
  return { x: view.x, y: stripTop - h, w: view.w, h };
}
