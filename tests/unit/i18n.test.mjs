// Static checks of every user-visible text (docs/i18n.md), for the browser client and the host:
//   - each side's three catalogs have the same keys, and each key the same {placeholders};
//   - keys are dotted, values are text, plurals come as `.one` and `.other` together, English
//     has no Chinese;
//   - the code has no Chinese outside comments: the web client's JS modules, index.html and
//     style.css, and the host's Rust sources and pages (every platform, tests excluded).
//
//   node --test tests/unit/i18n.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const web = join(repo, 'web', 'static');
const host = join(repo, 'host');
const LANGS = ['en', 'zh-Hans', 'zh-Hant'];
// Han, CJK punctuation, full-width forms, kana and Hangul: none of it belongs in the code.
const CJK = /[⺀-⿿　-ヿ㐀-䶿一-鿿가-힯豈-﫿︰-﹏＀-￯]/;

// One check: passes when it found nothing wrong.
const check = (name, list) => test(name, () => assert.deepEqual(list.slice(0, 20), []));

const placeholders = (s) => [...new Set([...String(s).matchAll(/\{(\w+)\}/g)].map((m) => m[1]))].sort().join(',');

function checkCatalogs(side, dir) {
  const catalogs = Object.fromEntries(LANGS.map((l) => [l, JSON.parse(readFileSync(join(dir, `${l}.json`), 'utf8'))]));
  const keys = (l) => Object.keys(catalogs[l]);
  const all = [...new Set(LANGS.flatMap(keys))].sort();
  const missing = [];
  for (const k of all) for (const l of LANGS) if (!(k in catalogs[l])) missing.push(`${l} lacks ${k}`);
  check(`${side}: the catalogs have the same keys (${keys('en').length})`, missing);
  const differ = [];
  for (const k of keys('en')) {
    const want = placeholders(catalogs.en[k]);
    for (const l of LANGS) if (k in catalogs[l] && placeholders(catalogs[l][k]) !== want) differ.push(`${k}: en {${want}} but ${l} {${placeholders(catalogs[l][k])}}`);
  }
  check(`${side}: each key has the same {placeholders} in every language`, differ);
  const bad = [];
  for (const l of LANGS) for (const [k, v] of Object.entries(catalogs[l])) {
    if (typeof v !== 'string' || !v.trim()) bad.push(`${l} ${k}: empty or not text`);
    if (!/^[a-z][A-Za-z]*(\.[A-Za-z][\w-]*)+$/.test(k)) bad.push(`${l} ${k}: not a dotted key`);
  }
  // One plural convention on both sides: `key.one` and `key.other`, always together.
  for (const k of keys('en')) {
    if (k.endsWith('.one') && !(`${k.slice(0, -4)}.other` in catalogs.en)) bad.push(`${k} without .other`);
    if (k.endsWith('.other') && !(`${k.slice(0, -6)}.one` in catalogs.en)) bad.push(`${k} without .one`);
    if (/_(one|other)$/.test(k)) bad.push(`${k}: plurals are .one and .other`);
  }
  // English shows no Chinese, except where a language is named in its own name.
  for (const [k, v] of Object.entries(catalogs.en)) if (CJK.test(v) && !k.startsWith('lang.')) bad.push(`en ${k}: ${v}`);
  check(`${side}: every value is text, keys are dotted, plurals are complete, English has no Chinese`, bad);
}

checkCatalogs('web', join(web, 'i18n'));
checkCatalogs('host', join(host, 'i18n'));

// JS with its comments taken out. Strings, template literals and regular expressions are
// kept, so a comment marker inside one is not taken for a comment.
function stripJs(src) {
  let out = '';
  let i = 0;
  let prev = ''; // the last significant character, to tell a regex from a division
  while (i < src.length) {
    const c = src[i];
    const n = src[i + 1];
    if (c === '/' && n === '/') { while (i < src.length && src[i] !== '\n') i++; continue; }
    if (c === '/' && n === '*') { const e = src.indexOf('*/', i + 2); i = e < 0 ? src.length : e + 2; continue; }
    if (c === '"' || c === "'" || c === '`') {
      let j = i + 1;
      while (j < src.length && src[j] !== c) j += src[j] === '\\' ? 2 : 1;
      out += src.slice(i, j + 1);
      i = j + 1;
      prev = c;
      continue;
    }
    if (c === '/' && (prev === '' || '(,=:[!&|?{};+-*%<>~^'.includes(prev) || /\b(return|typeof|case)$/.test(out.trimEnd()))) {
      let j = i + 1;
      let cls = false;
      while (j < src.length && src[j] !== '\n' && (cls || src[j] !== '/')) {
        if (src[j] === '\\') j++;
        else if (src[j] === '[') cls = true;
        else if (src[j] === ']') cls = false;
        j++;
      }
      out += src.slice(i, j + 1);
      i = j + 1;
      prev = '/';
      continue;
    }
    out += c;
    if (!/\s/.test(c)) prev = c;
    i++;
  }
  return out;
}

// Rust with its comments (nested block comments too) and its `#[cfg(test)] mod tests` part
// taken out. Strings, raw strings and char literals are kept; a lifetime is not a char.
function stripRust(src) {
  src = src.replace(/\r\n/g, '\n');
  const t = src.search(/#\[cfg\(test\)\]\s*\n\s*mod tests\b/);
  if (t >= 0) src = src.slice(0, t);
  let out = '';
  let i = 0;
  const word = (ch) => /[A-Za-z0-9_]/.test(ch || '');
  while (i < src.length) {
    const c = src[i];
    const n = src[i + 1];
    if (c === '/' && n === '/') { while (i < src.length && src[i] !== '\n') i++; continue; }
    if (c === '/' && n === '*') {
      let depth = 0;
      while (i < src.length) {
        if (src[i] === '/' && src[i + 1] === '*') { depth++; i += 2; } else if (src[i] === '*' && src[i + 1] === '/') { depth--; i += 2; if (!depth) break; } else i++;
      }
      continue;
    }
    // r"…", r#"…"#, br"…": no escapes inside, ended by a quote and as many hashes.
    if (c === 'r' && (n === '"' || n === '#') && (!word(src[i - 1]) || src[i - 1] === 'b')) {
      let j = i + 1;
      while (src[j] === '#') j++;
      if (src[j] === '"') {
        const end = src.indexOf(`"${'#'.repeat(j - i - 1)}`, j + 1);
        const stop = end < 0 ? src.length : end + 1 + (j - i - 1);
        out += src.slice(i, stop);
        i = stop;
        continue;
      }
    }
    if (c === '"') {
      let j = i + 1;
      while (j < src.length && src[j] !== '"') j += src[j] === '\\' ? 2 : 1;
      out += src.slice(i, j + 1);
      i = j + 1;
      continue;
    }
    if (c === "'") {
      // 'x', '\n', '\u{4e2d}', or a lifetime ('a), which has no closing quote.
      let end = -1;
      if (n === '\\') end = src.indexOf("'", i + 2) + 1;
      else if (src[i + 2] === "'") end = i + 3;
      else if (n && src.codePointAt(i + 1) > 0xffff && src[i + 3] === "'") end = i + 4;
      if (end > 0) { out += src.slice(i, end); i = end; continue; }
    }
    out += c;
    i++;
  }
  return out;
}

const stripCss = (src) => src.replace(/\/\*[\s\S]*?\*\//g, '');
// A page: no <!-- --> comments, and its script and style without their comments.
const stripHtml = (src) => src.replace(/<!--[\s\S]*?-->/g, '')
  .replace(/(<script\b[^>]*>)([\s\S]*?)(<\/script>)/g, (m, a, body, z) => a + stripJs(body) + z)
  .replace(/(<style\b[^>]*>)([\s\S]*?)(<\/style>)/g, (m, a, body, z) => a + stripCss(body) + z);

const files = (dir, test) => readdirSync(dir, { recursive: true }).map((f) => String(f).replace(/\\/g, '/')).filter(test).sort();
const noChinese = (base, label, list) => {
  if (!list.length) check(`${label}: there is code to scan`, ['none found']);
  for (const [file, strip] of list) {
    const code = strip(readFileSync(join(base, file), 'utf8'));
    const found = code.split('\n').map((line) => line.trim()).filter((line) => CJK.test(line)).map((line) => line.slice(0, 120));
    check(`${label}/${file} has no Chinese outside comments`, found);
  }
};

// Every module of the client, wherever it sits under js/.
noChinese(web, 'web', [
  ...files(join(web, 'js'), (f) => f.endsWith('.js')).map((f) => [`js/${f}`, stripJs]),
  ['index.html', stripHtml],
  ['style.css', stripCss],
]);
// Every host source on every platform. `tests.rs` files are test modules of their own.
noChinese(join(host, 'src'), 'host', [
  ...files(join(host, 'src'), (f) => f.endsWith('.rs') && !/(^|\/)tests\.rs$/.test(f)).map((f) => [f, stripRust]),
  ...files(join(host, 'src'), (f) => f.endsWith('.html')).map((f) => [f, stripHtml]),
]);

// The strippers themselves: what they keep and what they drop.
{
  const count = (s) => [...s].filter((ch) => CJK.test(ch)).length;
  const rust = stripRust('let a = "// 中"; // 注释\n/* 注释 /* 嵌套 */ */ let b = r#"中"#; let c = \'"\'; let l: &\'a str = "中";\n#[cfg(test)]\nmod tests { "中" }');
  const html = stripHtml('<!-- 注释 --><p>中</p><script>\n// 注释\nconst a = \'中\'; /* 注释 */\n</script><style>/* 注释 */</style>');
  check('the scanners see strings, not comments or tests', [
    ...(count(rust) === 3 ? [] : [`rust kept ${count(rust)} of 3: ${rust}`]),
    ...(count(html) === 2 ? [] : [`html kept ${count(html)} of 2: ${html}`]),
  ]);
}
