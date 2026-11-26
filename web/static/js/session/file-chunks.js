import { DATA_CHUNK } from '../proto.js';

export function readFileChunk(file, offset, length = DATA_CHUNK) {
  return file.slice(offset, offset + length).arrayBuffer();
}
