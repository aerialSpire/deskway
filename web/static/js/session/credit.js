// Outstanding bytes count against the receiver's window until it consumes them.
export class Credit {
  constructor(window) {
    this.window = window;
    this.outstanding = 0;
  }

  get available() { return this.window - this.outstanding; }

  take(bytes) {
    if (bytes <= 0 || bytes > this.available) return false;
    this.outstanding += bytes;
    return true;
  }

  acknowledge(bytes) {
    if (bytes <= 0 || bytes > this.outstanding) return false;
    this.outstanding -= bytes;
    return true;
  }
}
