// Deskway as an app: the shell kept for offline (sw.js), and the browser's own offer to install
// it. What the account menu makes of that is views/account.js's; what an installed app is,
// env.js's.
//
// Public surface: startPwa(changed), canPromptInstall(), promptInstall().

let installPrompt = null;

// `changed` redraws the page when installing becomes possible or is done.
export function startPwa(changed) {
  if ('serviceWorker' in navigator) navigator.serviceWorker.register('sw.js').catch(() => {});
  // Chromium offers to install; the menu asks for that prompt when the person chooses to.
  addEventListener('beforeinstallprompt', (e) => { e.preventDefault(); installPrompt = e; changed(); });
  addEventListener('appinstalled', () => { installPrompt = null; changed(); });
}

export const canPromptInstall = () => !!installPrompt;

// The browser's install prompt; it can be shown once.
export async function promptInstall() {
  const p = installPrompt;
  installPrompt = null;
  if (p) await p.prompt().catch(() => {});
}
