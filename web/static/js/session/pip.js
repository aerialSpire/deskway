// Picture in picture (docs/session-lifecycle.md, "Picture in picture"): when the page goes to the
// background, the session's picture floats over the other apps, where the browser does that by
// itself (Media Session `enterpictureinpicture`); back in front, the picture is in the page again.
// Nothing to press: it only follows the page going away and coming back. A floating picture keeps
// the page and the session running; input waits until the page is in front again (held keys are
// let go when it is hidden, session.js).
//
// The browser's standard API (`requestPictureInPicture`), or Safari's presentation mode where only
// that exists. Nothing happens where neither is.
//
// Public surface:
//   new Pip(video)    video: the session's <video>
//   pip.active        the picture floats now
//   pip.enter()       floats the picture (what the browser calls as the page goes to the background)
//   pip.destroy()     the session ended: the picture is back in the page, nothing is listened to

const WEBKIT_PIP = 'picture-in-picture';

// How this browser floats a video: 'standard', 'webkit' or null.
function kind(video) {
  if (document.pictureInPictureEnabled && typeof video.requestPictureInPicture === 'function') return 'standard';
  if (typeof video.webkitSupportsPresentationMode === 'function' && video.webkitSupportsPresentationMode(WEBKIT_PIP)) return 'webkit';
  return null;
}

// Media Session actions a browser does not know throw.
function setAction(action, handler) {
  try { navigator.mediaSession?.setActionHandler(action, handler); } catch { /* not supported here */ }
}

export class Pip {
  constructor(video) {
    this.video = video;
    this.kind = kind(video);
    if (!this.kind) return;
    // Back in front: the picture returns to the page.
    this.onVisible = () => { if (!document.hidden && this.active) this.leave(); };
    document.addEventListener('visibilitychange', this.onVisible);
    // Browsers that float a playing video by themselves when the page goes to the background.
    setAction('enterpictureinpicture', () => this.enter());
  }

  get active() {
    if (this.kind === 'standard') return document.pictureInPictureElement === this.video;
    if (this.kind === 'webkit') return this.video.webkitPresentationMode === WEBKIT_PIP;
    return false;
  }

  enter() {
    if (this.kind === 'standard') this.video.requestPictureInPicture().catch(() => { /* no picture yet, or not allowed now */ });
    else if (this.kind === 'webkit') this.video.webkitSetPresentationMode(WEBKIT_PIP);
  }

  leave() {
    if (this.kind === 'standard') document.exitPictureInPicture().catch(() => { /* already back */ });
    else if (this.kind === 'webkit') this.video.webkitSetPresentationMode('inline');
  }

  destroy() {
    if (!this.kind) return;
    if (this.active) this.leave();
    setAction('enterpictureinpicture', null);
    document.removeEventListener('visibilitychange', this.onVisible);
  }
}
