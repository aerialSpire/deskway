// Incremental SHA-256 (FIPS 180-4). Web Crypto's digest requires the complete input.
const INITIAL = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
const ROUND = [
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];
const rotate = (x, n) => (x >>> n) | (x << (32 - n));

export class Sha256 {
  constructor() {
    this.state = new Uint32Array(INITIAL);
    this.words = new Uint32Array(64);
    this.tail = new Uint8Array(64);
    this.length = 0;
  }

  update(bytes) {
    let used = this.length % 64;
    this.length += bytes.length;
    let at = 0;
    if (used) {
      const count = Math.min(64 - used, bytes.length);
      this.tail.set(bytes.subarray(0, count), used);
      used += count;
      if (used < 64) return;
      this.block(new DataView(this.tail.buffer), 0);
      at = count;
    }
    const data = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    for (; at + 64 <= bytes.length; at += 64) this.block(data, at);
    this.tail.set(bytes.subarray(at), 0);
  }

  block(data, at) {
    const w = this.words;
    for (let i = 0; i < 16; i++) w[i] = data.getUint32(at + 4 * i);
    for (let i = 16; i < 64; i++) {
      const x = w[i - 15], y = w[i - 2];
      const s0 = rotate(x, 7) ^ rotate(x, 18) ^ (x >>> 3);
      const s1 = rotate(y, 17) ^ rotate(y, 19) ^ (y >>> 10);
      w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }
    let [a, b, c, d, e, f, g, h] = this.state;
    for (let i = 0; i < 64; i++) {
      const s1 = rotate(e, 6) ^ rotate(e, 11) ^ rotate(e, 25);
      const choice = (e & f) ^ (~e & g);
      const first = (h + s1 + choice + ROUND[i] + w[i]) | 0;
      const s0 = rotate(a, 2) ^ rotate(a, 13) ^ rotate(a, 22);
      const majority = (a & b) ^ (a & c) ^ (b & c);
      h = g; g = f; f = e; e = (d + first) | 0;
      d = c; c = b; b = a; a = (first + s0 + majority) | 0;
    }
    this.state[0] += a; this.state[1] += b; this.state[2] += c; this.state[3] += d;
    this.state[4] += e; this.state[5] += f; this.state[6] += g; this.state[7] += h;
  }

  // Finalize once. The transfer retains the digest if its final message must be replayed.
  finish() {
    const used = this.length % 64;
    const padding = new Uint8Array(used < 56 ? 64 - used : 128 - used);
    padding[0] = 0x80;
    new DataView(padding.buffer).setBigUint64(padding.length - 8, BigInt(this.length) * 8n);
    this.update(padding);
    return Array.from(this.state, (word) => word.toString(16).padStart(8, '0')).join('');
  }
}
