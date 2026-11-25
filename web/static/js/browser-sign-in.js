// Wait for the browser's device key before letting it use an approved session. A waiting
// browser prepares the same key as soon as it has a name, so its approver can check the code.
export function browserSignIn({ getSession, ensurePasskey, reportPasskey, keyCode }) {
  async function prepare(name) {
    const key = await ensurePasskey(name).catch(() => null);
    return key && await reportPasskey(key) ? key : null;
  }

  return {
    async current() {
      const res = await getSession();
      if (res.ok || res.status === 428) await prepare();
      return res;
    },
    async waiting(name, fallback) {
      try {
        const key = await prepare(name);
        if (key) return { code: await keyCode(key.key), noKey: false };
      } catch { /* the approver has not received a key to compare */ }
      return { code: fallback, noKey: true };
    },
  };
}
