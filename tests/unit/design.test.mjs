// Design check: every stylesheet keeps to the tokens of web/static/design.css (docs/design.md).
// Colours, font sizes and families, line heights, radii, spacing and shadows are written as
// tokens; only design.css itself holds values. Run: node --test tests/unit/design.test.mjs.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { extname, join } from 'node:path';

const root = new URL('../../', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1');
const read = (p) => readFileSync(join(root, p), 'utf8');


// The stylesheets held to the standard: the client's, and every <style> in the host's windows.
const VENDORED = new Map([
  ['vendor/xterm.css', 'The upstream terminal stylesheet is checked in byte for byte.'],
]);
const sheets = readdirSync(join(root, 'web/static'), { recursive: true, withFileTypes: true })
  .filter((f) => f.isFile() && extname(f.name) === '.css')
  .map((f) => join(f.parentPath, f.name).slice(join(root, 'web/static').length + 1).replaceAll('\\', '/'))
  .filter((f) => f !== 'design.css' && !VENDORED.has(f))
  .map((f) => [`web/static/${f}`, read(`web/static/${f}`)]);
for (const f of readdirSync(join(root, 'host/src/ui')).filter((f) => f.endsWith('.html'))) {
  const html = read(`host/src/ui/${f}`);
  for (const m of html.matchAll(/<style>([\s\S]*?)<\/style>/g)) sheets.push([`host/src/ui/${f}`, m[1]]);
}

// Exceptions, each with its reason (docs/design.md, Exceptions).
const EXCEPTIONS = [
  // The on-screen keyboard keeps a keyboard's own proportions: key heights, label sizes, gaps.
  /^\.(keypad|kb-)|\.keypad\b|\.kb-/,
  // Drawn icons: the clipboard ball's clipboard, the remote pointer's outline.
  /^\.ball-icon/,
  /^\.rcursor/,
];
const excepted = (selector) => EXCEPTIONS.some((re) => re.test(selector.trim()));

// Every rule of a stylesheet: its selector and declarations (innermost blocks of @media too).
function rules(css) {
  const out = [];
  const text = css.replace(/\/\*[\s\S]*?\*\//g, '');
  for (const m of text.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const selector = m[1].trim();
    if (selector.startsWith('@') || /^(from|to|\d+%)$/.test(selector)) continue;
    for (const d of m[2].split(';')) {
      const i = d.indexOf(':');
      if (i < 0) continue;
      out.push({ selector, prop: d.slice(0, i).trim().toLowerCase(), value: d.slice(i + 1).trim() });
    }
  }
  return out;
}

const COLOUR = /#[0-9a-f]{3,8}\b|\brgba?\(|\bhsla?\(/i;
// A length outside the scale: a number with a unit, other than 0 and 1px hairlines.
const LENGTH = /(?<![\w-])-?\d*\.?\d+(px|rem|em)\b/g;
const SPACING_OK = (v) => [...v.matchAll(LENGTH)].every((m) => m[0] === '1px' || m[0] === '0px');

const CHECKS = [
  ['colour', () => true, (v) => !COLOUR.test(v), 'colours are tokens (var(--…))'],
  ['font-size', (p) => p === 'font-size', (v) => /^(var\(--fs-[\w-]+\)|inherit)$/.test(v), 'font sizes are --fs-*'],
  ['font', (p) => p === 'font', (v) => !LENGTH.test(v) || /var\(--fs-/.test(v), 'a font shorthand takes --fs-*'],
  ['font-family', (p) => p === 'font-family', (v) => /^(var\(--font-[\w-]+\)|inherit)$/.test(v), 'font families are --font-*'],
  ['line-height', (p) => p === 'line-height', (v) => /^(var\(--lh-[\w-]+\)|1|normal|inherit)$/.test(v), 'line heights are --lh-*'],
  ['border-radius', (p) => p === 'border-radius', (v) => v.split(/\s+/).every((x) => /^(var\(--r-[\w-]+\)|0|50%)$/.test(x)), 'radii are --r-* (or 50% for circles)'],
  ['spacing', (p) => /^(padding|margin|gap|row-gap|column-gap)(-(top|right|bottom|left|inline|block))?$/.test(p), SPACING_OK, 'spacing is --s* (0 and 1px allowed)'],
  ['shadow', (p) => p === 'box-shadow', (v) => /^(none|var\(--shadow-[\w-]+\))$/.test(v), 'shadows are --shadow-*'],
];

for (const [file, css] of sheets) {
  test(`${file} keeps to the design tokens`, () => {
    const bad = [];
    for (const r of rules(css)) {
      if (excepted(r.selector)) continue;
      for (const [name, applies, ok, why] of CHECKS) {
        LENGTH.lastIndex = 0;
        if (applies(r.prop) && !ok(r.value)) bad.push(`  ${r.selector} { ${r.prop}: ${r.value} }  (${name}: ${why})`);
      }
    }
    assert.equal(bad.length, 0, `${bad.length} values outside the standard:\n${bad.join('\n')}`);
  });
}

test('the host embeds design.css itself, not a copy', () => {
  for (const f of readdirSync(join(root, 'host/src/ui')).filter((f) => f.endsWith('.html'))) {
    assert.ok(!/--ground:\s*#/.test(read(`host/src/ui/${f}`)), `${f} defines tokens of its own`);
  }
});

test('file names preserve spaces in breadcrumbs, rows, link targets and confirmations', () => {
  const selectors = rules(read('web/static/style.css'))
    .filter(({ prop, value }) => prop === 'white-space-collapse' && value === 'preserve')
    .flatMap(({ selector }) => selector.split(',').map((part) => part.trim()));
  for (const selector of ['.files-path .btn', '.files-name', '.files-target', '.files-confirm']) {
    assert.ok(selectors.includes(selector), selector);
  }
});
