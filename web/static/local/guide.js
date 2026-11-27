export function showGuide(language, catalogs) {
  const text = (key) => catalogs[language]?.[key] ?? catalogs.en[key];
  document.documentElement.lang = language;
  document.title = text('guide.title');
  for (const el of document.querySelectorAll('[data-text]')) el.textContent = text(el.dataset.text);

  const agent = navigator.userAgent;
  // iPadOS can use the desktop Safari user agent.
  const ios = /iPhone|iPad|iPod/.test(agent) || /Macintosh/.test(agent) && navigator.maxTouchPoints > 1;
  const platform = ios ? 'ios' : /Android/.test(agent) ? 'android' : 'other';
  document.getElementById('download').href = ios ? '/local/ca.mobileconfig' : '/local/ca.crt';
  document.getElementById('install').textContent = text(`guide.${platform}.install`);
  document.getElementById('trust').hidden = !ios;

  const https = new URL(`https://${location.hostname}/`);
  https.port = document.body.dataset.httpsPort;
  document.getElementById('continue').href = https.href;
  document.querySelector('main').hidden = false;
}
