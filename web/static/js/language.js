// Prefer the browser's languages in order. Traditional Chinese follows Taiwan wording.
export function browserLanguage() {
  const wanted = navigator.languages && navigator.languages.length ? navigator.languages : [navigator.language || ''];
  for (const l of wanted) {
    const x = String(l || '').toLowerCase().replace(/_/g, '-');
    if (!x) continue;
    if (/^zh-(tw|hk|mo)(-|$)|^zh-hant(-|$)/.test(x)) return 'zh-Hant';
    if (x === 'zh' || x.startsWith('zh-')) return 'zh-Hans';
    return 'en';
  }
  return 'en';
}
