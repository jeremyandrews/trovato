// B160: the Pagefind (JavaScript on) search path escapes exactly once.
//
// Pagefind's `excerpt` is HTML: the page text with `&`, `<`, `>` and quotes
// already escaped, `<mark>` around the matches. scolta.js used to run
// escapeHtml(stripHtml(excerpt)) over it, so "Tom &amp; Jerry" in the excerpt
// reached the visitor as the visible text "Tom &amp; Jerry".
//
// Run with Node's own test runner, no packages: `node --test static/js/test/`.

'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');

// scolta.js attaches `Scolta` to `window`, or to `this` outside a browser,
// which for a CommonJS module is its exports.
const { Scolta } = require(path.join(__dirname, '..', 'scolta.js'));
const { escapeHtml, htmlToText, highlightText, bestExcerpt } = Scolta._text;

test('a Pagefind excerpt is read as HTML and written escaped once', () => {
  const excerpt = 'Tom &amp; Jerry &lt;3 &quot;café&quot; <mark>noank</mark> &#39;s';
  const text = htmlToText(excerpt);
  assert.equal(text, 'Tom & Jerry <3 "café" noank \'s');

  const html = highlightText(text, ['noank']);
  assert.equal(
    html,
    'Tom &amp; Jerry &lt;3 &quot;café&quot; <mark>noank</mark> &#39;s',
  );
  assert.ok(!html.includes('&amp;amp;'), 'escaped twice: ' + html);
});

test('bestExcerpt returns text the renderer escapes once', () => {
  const data = {
    excerpt: 'Sailing to <mark>Noank</mark> &amp; Mystic',
    content: 'Sailing to Noank &amp; Mystic',
    meta: { title: 'Noank &amp; Mystic' },
  };
  const excerpt = bestExcerpt(data, 200, ['noank']);
  assert.equal(excerpt, 'Sailing to Noank & Mystic');
  assert.equal(
    highlightText(excerpt, ['noank']),
    'Sailing to <mark>Noank</mark> &amp; Mystic',
  );
});

test('a term cannot match inside an entity or inject markup', () => {
  // "amp" is in "&amp;" once escaped; matching runs on the text, so it is not.
  assert.equal(highlightText('Tom & Jerry', ['amp']), 'Tom &amp; Jerry');
  assert.equal(
    highlightText('a <b> tag', ['<b>']),
    'a <mark>&lt;b&gt;</mark> tag',
  );
  assert.equal(
    highlightText('x', ['<img src=x onerror=alert(1)>']),
    'x',
  );
});

test('markup in a Pagefind value never reaches the page as markup', () => {
  const text = htmlToText('&lt;script&gt;alert(1)&lt;/script&gt;');
  assert.equal(text, '<script>alert(1)</script>');
  assert.equal(escapeHtml(text), '&lt;script&gt;alert(1)&lt;/script&gt;');
  // Quotes are escaped too, so a value is safe inside an attribute.
  assert.equal(escapeHtml('" onmouseover="x'), '&quot; onmouseover=&quot;x');
});

test('character references decode once, unknown ones stay as written', () => {
  assert.equal(htmlToText('&amp;amp;'), '&amp;');
  assert.equal(htmlToText('&#x263A; &#9731; &bogus;'), '☺ ☃ &bogus;');
  assert.equal(htmlToText('1 < 2 and 3 > 2'), '1 < 2 and 3 > 2');
});
