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
const { escapeHtml, htmlToText, highlightText, bestExcerpt, formatInline } = Scolta._text;

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

// B218: the index is updated in place, and a file a tab's old meta names is
// deleted 24 hours after it stops being referenced. A search that fails on one
// reloads the index (destroy, init) once and runs again.
const { withIndexReload, setPagefind } = Scolta._index;

function fakePagefind() {
  const calls = [];
  return {
    calls,
    destroy: async () => calls.push('destroy'),
    init: async () => calls.push('init'),
  };
}

test('a failed search reloads the index once and runs again', async () => {
  const pf = fakePagefind();
  setPagefind(pf);
  let runs = 0;
  const result = await withIndexReload(async () => {
    runs += 1;
    if (runs === 1) throw new Error('404 index/en_abc.pf_index');
    return 'found';
  });
  assert.equal(result, 'found');
  assert.equal(runs, 2);
  assert.deepEqual(pf.calls, ['destroy', 'init']);
});

test('a search that fails again after the reload fails', async () => {
  const pf = fakePagefind();
  setPagefind(pf);
  await assert.rejects(
    withIndexReload(async () => {
      throw new Error('still broken');
    }),
    /still broken/,
  );
  assert.deepEqual(pf.calls, ['destroy', 'init'], 'one reload, not a loop');
});

test('searches failing at once share one reload', async () => {
  const pf = fakePagefind();
  setPagefind(pf);
  const failOnce = () => {
    let failed = false;
    return async () => {
      if (!failed) {
        failed = true;
        throw new Error('gone');
      }
      return 'ok';
    };
  };
  const results = await Promise.all([
    withIndexReload(failOnce()),
    withIndexReload(failOnce()),
    withIndexReload(failOnce()),
  ]);
  assert.deepEqual(results, ['ok', 'ok', 'ok']);
  assert.deepEqual(pf.calls, ['destroy', 'init']);
});

test('a search that works does not reload', async () => {
  const pf = fakePagefind();
  setPagefind(pf);
  assert.equal(await withIndexReload(async () => 1), 1);
  assert.deepEqual(pf.calls, []);
  setPagefind(null);
});

// B282: an AI summary can carry a planted `[text](url)` link (prompt
// injection), so the renderer — not the model — decides what is clickable.
// Only http(s) and same-site paths may become an <a href>; everything else,
// a scheme that runs script or loads data, or a protocol-relative URL to
// another origin, renders as plain text.
test('formatInline renders only http(s) and same-site links', () => {
  const link = (text) => `<a href="${text.url}" target="_blank" rel="noopener">${text.label}</a>`;

  assert.equal(
    // No parens in the payload: the link regex itself stops at the first
    // `)`, which is a separate, pre-existing limitation this test does not
    // exercise.
    formatInline("[click here](javascript:location='https://evil.example/')"),
    'click here',
    'a javascript: URL must never become an href',
  );
  assert.equal(
    formatInline('[click here](data:text/html;base64,PHNjcmlwdD4=)'),
    'click here',
    'a data: URL must never become an href',
  );
  assert.equal(
    formatInline('[click here](//evil.example/phish)'),
    'click here',
    'a protocol-relative URL is another origin, not a path',
  );
  assert.equal(
    formatInline('[the item](/item/x)'),
    link({ url: '/item/x', label: 'the item' }),
    'a single leading slash is a same-site path',
  );
  assert.equal(
    formatInline('[RustConf](https://rustconf.example/schedule)'),
    link({ url: 'https://rustconf.example/schedule', label: 'RustConf' }),
    'an https URL renders normally',
  );
});
