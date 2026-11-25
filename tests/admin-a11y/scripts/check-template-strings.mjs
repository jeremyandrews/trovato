// Static check: literal interface text in admin templates that does not go
// through `t()`. A quick, local companion to the rendered marker-mode test
// (crates/kernel/tests/admin_i18n_test.rs), which is the real gate.
//
//   node tests/admin-a11y/scripts/check-template-strings.mjs [files...]
//
// With no files, checks templates/page--admin.html and templates/admin/**.
// Reports text between tags, and aria-label, title, placeholder, alt and
// button value attributes, that contain a letter outside a Tera tag.
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';

const root = new URL('../../../', import.meta.url).pathname;

function walk(dir) {
  return readdirSync(dir).flatMap((name) => {
    const p = join(dir, name);
    return statSync(p).isDirectory() ? walk(p) : p.endsWith('.html') ? [p] : [];
  });
}

const files = process.argv.length > 2
  ? process.argv.slice(2)
  : [join(root, 'templates/page--admin.html'), ...walk(join(root, 'templates/admin'))];

// Remove Tera comments, tags and expressions, and <script>/<style> bodies.
function strip(src) {
  return src
    .replace(/\{#[\s\S]*?#\}/g, (m) => m.replace(/[^\n]/g, ' '))
    .replace(/<script\b[\s\S]*?<\/script>/gi, (m) => m.replace(/[^\n]/g, ' '))
    .replace(/<style\b[\s\S]*?<\/style>/gi, (m) => m.replace(/[^\n]/g, ' '))
    .replace(/\{\{[\s\S]*?\}\}/g, (m) => m.replace(/[^\n]/g, ' '))
    .replace(/\{%[\s\S]*?%\}/g, (m) => m.replace(/[^\n]/g, ' '))
    .replace(/<!--[\s\S]*?-->/g, (m) => m.replace(/[^\n]/g, ' '));
}

const letter = /\p{L}/u;
let problems = 0;
for (const file of files) {
  const src = readFileSync(file, 'utf8');
  const s = strip(src);
  const report = (index, what) => {
    const line = s.slice(0, index).split('\n').length;
    console.log(`${relative(root, file)}:${line}: ${what}`);
    problems++;
  };
  // Text nodes: between '>' and '<'.
  const textRe = />([^<>]*)</g;
  let m;
  while ((m = textRe.exec(s))) {
    const text = m[1].replace(/&[a-z]+;|&#\d+;/gi, ' ');
    if (letter.test(text)) report(m.index, `text "${m[1].trim().slice(0, 60)}"`);
  }
  // Attributes that are read out or shown.
  const attrRe = /\b(aria-label|title|placeholder|alt|aria-description|data-confirm)\s*=\s*"([^"]*)"/g;
  while ((m = attrRe.exec(s))) {
    if (letter.test(m[2])) report(m.index, `${m[1]}="${m[2].trim().slice(0, 60)}"`);
  }
  const valueRe = /<input\b[^>]*type="(?:submit|button|reset)"[^>]*>/g;
  while ((m = valueRe.exec(s))) {
    const v = /\bvalue\s*=\s*"([^"]*)"/.exec(m[0]);
    if (v && letter.test(v[1])) report(m.index, `button value="${v[1]}"`);
  }
}
console.log(problems ? `${problems} untranslated strings` : 'no untranslated strings');
process.exit(problems ? 1 : 0);
