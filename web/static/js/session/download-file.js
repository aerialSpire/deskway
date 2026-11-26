// The fallback Blob is bounded on browsers without a local writable file picker.
export const DOWNLOAD_LIMIT = 64 * 1024 * 1024;

export async function chooseDownload(name, api = globalThis) {
  if (typeof api.showSaveFilePicker === 'function') {
    // Invoke the picker in the menu click. Open the writer only when this job reaches the front.
    const handle = await api.showSaveFilePicker({ suggestedName: name });
    return {
      async open() {
        const stream = await handle.createWritable();
        return {
          write: (bytes) => stream.write(bytes),
          close: () => stream.close(),
          abort: () => stream.abort(),
        };
      },
    };
  }
  return {
    limit: DOWNLOAD_LIMIT,
    open(size, save = saveBlob) {
      if (size > DOWNLOAD_LIMIT) return null;
      const parts = [];
      return {
        write: (bytes) => parts.push(bytes),
        close: () => {
          const blob = new Blob(parts, { type: 'application/octet-stream' });
          parts.length = 0;
          return () => save(blob, name);
        },
        abort: () => { parts.length = 0; },
      };
    },
  };
}

function saveBlob(blob, name) {
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = name;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
