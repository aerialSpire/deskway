// Names and metadata from the browse channel, independent of the page.
const utf8 = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });

export function decodeName(key) {
  const bytes = Uint8Array.from(atob(key.replaceAll('-', '+').replaceAll('_', '/')), (ch) => ch.charCodeAt(0));
  try {
    return { value: utf8.decode(bytes) };
  } catch {
    const escaped = Array.from(bytes, (b) => b >= 32 && b <= 126 && b !== 92
      ? String.fromCharCode(b) : '\\x' + b.toString(16).padStart(2, '0')).join('');
    return { escaped };
  }
}

export function safeText(text) {
  return text.replace(/[\\\p{Cc}\p{Cf}\p{Zl}\p{Zp}]/gu, (char) => char === '\\' ? '\\\\'
    : '\\u{' + char.codePointAt(0).toString(16) + '}');
}

export function displayName(key) {
  const { value, escaped } = decodeName(key);
  return escaped ?? safeText(value);
}

export function crumbs(path) {
  return [{ label: '/', path: [] }, ...path.map((key, i) => ({ label: displayName(key), path: path.slice(0, i + 1) }))];
}

export function entryActions({ kind, opens }) {
  return [...(kind === 'folder' || opens ? ['open'] : []), 'rename', 'delete'];
}

export function permissions({ kind, mode }) {
  const type = { folder: 'd', file: '-', link: 'l', special: '?' }[kind];
  return type + [6, 3, 0].map((shift, i) => {
    const bits = mode >> shift;
    const special = mode & (0o4000 >> i);
    const exec = bits & 1;
    const x = special ? (i === 2 ? (exec ? 't' : 'T') : (exec ? 's' : 'S')) : (exec ? 'x' : '-');
    return (bits & 4 ? 'r' : '-') + (bits & 2 ? 'w' : '-') + x;
  }).join('');
}
