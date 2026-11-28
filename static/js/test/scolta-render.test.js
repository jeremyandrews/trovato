// B170b: the results header counts only the hits the page renders.
//
// Pagefind answers a term it has no word for with the longest indexed word
// the term starts with (pagefind_web `find_word_extensions`, "checking the
// inverse"): "kerneltrap" returns every page with "kernel". scolta.js keeps
// such a hit off the page (bestExcerpt returns null when the term is in none
// of excerpt, content or title), but it counted the header, "Showing N" and
// "Show more" from the raw hits, so a search could read "11 results" over an
// empty list. These tests run the real search path, from the query to the
// rendered cards, against a fake Pagefind and the few DOM calls scolta.js
// makes.
//
// Run with Node's own test runner, no packages: `node --test static/js/test/`.

'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');

// One stand-in element: the properties and methods scolta.js touches.
function fakeElement(id) {
  return {
    id,
    innerHTML: '',
    value: '',
    hidden: false,
    className: '',
    style: { display: '' },
    dataset: {},
    addEventListener() {},
    insertAdjacentHTML(_where, html) {
      this.innerHTML += html;
    },
    closest() {
      return null;
    },
  };
}

const elements = new Map();
const root = fakeElement('scolta-search');
root.querySelector = (selector) => {
  const id = selector.replace(/^#/, '');
  if (!elements.has(id)) elements.set(id, fakeElement(id));
  return elements.get(id);
};
const el = (id) => root.querySelector('#' + id);

// The hits the fake Pagefind answers with, set per test.
let hits = [];
globalThis.__scoltaFakeHits = () => hits;

globalThis.document = {
  querySelector: (selector) => (selector === '#scolta-search' ? root : null),
};
globalThis.window = {
  location: { search: '' },
  scolta: {
    // A data: URL module, so initPagefind's dynamic import loads the fake.
    pagefindPath:
      'data:text/javascript,' +
      encodeURIComponent(
        'export async function init() {}\n' +
          'export async function destroy() {}\n' +
          'export async function search() {\n' +
          '  const hits = globalThis.__scoltaFakeHits();\n' +
          '  return { results: hits.map((data) => ({ data: async () => data })) };\n' +
          '}\n',
      ),
    scoring: { AI_EXPAND_QUERY: false, AI_SUMMARIZE: false },
  },
};

require(path.join(__dirname, '..', 'scolta.js'));
const { Scolta } = globalThis.window;

// Keep the search path's progress logging out of the test output.
console.log = () => {};

// A page Pagefind returns for "kerneltrap" by its inverse-prefix fallback:
// the indexed word is "kernel", "kerneltrap" is nowhere on the page.
function prefixOnlyHit(n) {
  return {
    url: `/static/prefix-${n}.html`,
    excerpt: `the <mark>kernel</mark> boot takes ${n} ms`,
    content: `Performance notes ${n}. The kernel boot takes ${n} ms.`,
    meta: { title: `Boot timing note${n}x`, url: `/item/prefix-${n}` },
  };
}

// A page Pagefind returns for "connections" by stemming: both words stem to
// "connect", but "connections" is nowhere on the page. Stemming is how
// skipped hits and real ones end up in one result set; the inverse-prefix
// fallback only answers a term no page has a word starting with.
function stemOnlyHit(n) {
  return {
    url: `/static/stem-${n}.html`,
    excerpt: `the pool <mark>connected</mark> in ${n} ms`,
    content: `Pool notes ${n}. The pool connected in ${n} ms.`,
    meta: { title: `Pool timing note${n}x`, url: `/item/stem-${n}` },
  };
}

// A page that really contains "connections".
function connectionsHit(n) {
  return {
    url: `/static/conn-${n}.html`,
    excerpt: `too many <mark>connections</mark> on server ${n}`,
    content: `Server ${n}. Too many connections on server ${n}.`,
    meta: { title: `Server volume${n}x issue${n}y`, url: `/item/conn-${n}` },
  };
}

function cardCount() {
  return (el('scolta-results').innerHTML.match(/class="scolta-result-card"/g) || []).length;
}

function headerCount() {
  const m = el('scolta-results-header').innerHTML.match(/<span>([\d,]+) results for/);
  return m ? Number(m[1].replace(/,/g, '')) : null;
}

function headerShowing() {
  const m = el('scolta-results-header').innerHTML.match(/Showing (\d+)/);
  return m ? Number(m[1]) : null;
}

async function search(query) {
  el('scolta-query').value = query;
  await Scolta.doSearch();
}

test.before(async () => {
  await Scolta.init('#scolta-search');
});

test('the header counts the cards a search with skipped hits renders', async () => {
  // Three real hits among eight stem-only ones.
  hits = [];
  for (let i = 0; i < 8; i++) hits.push(stemOnlyHit(i));
  hits.splice(2, 0, connectionsHit(0));
  hits.splice(6, 0, connectionsHit(1));
  hits.push(connectionsHit(2));

  await search('connections');

  assert.equal(cardCount(), 3, el('scolta-results').innerHTML);
  assert.equal(headerCount(), cardCount(), el('scolta-results-header').innerHTML);
  assert.equal(headerShowing(), cardCount());
  assert.equal(el('scolta-load-more').style.display, 'none', 'nothing more to show');
  assert.equal(el('scolta-no-results').style.display, 'none');
});

test('every page fills to RESULTS_PER_PAGE cards when enough real hits exist', async () => {
  // 23 real hits, with a stem-only hit after every second one, so each raw
  // slice of ten holds skipped hits.
  hits = [];
  for (let i = 0; i < 23; i++) {
    hits.push(connectionsHit(i));
    if (i % 2 === 1) hits.push(stemOnlyHit(i));
  }

  await search('connections');
  assert.equal(headerCount(), 23, el('scolta-results-header').innerHTML);
  assert.equal(cardCount(), 10, 'first page');
  assert.equal(headerShowing(), 10);
  assert.equal(el('scolta-load-more').style.display, 'block');

  Scolta.showMore();
  assert.equal(cardCount(), 20, 'second page');
  assert.equal(headerShowing(), 20);
  assert.equal(el('scolta-load-more').style.display, 'block');

  Scolta.showMore();
  assert.equal(cardCount(), 23, 'last page');
  assert.equal(headerShowing(), 23);
  assert.equal(headerCount(), 23);
  assert.equal(el('scolta-load-more').style.display, 'none');
});

test('a search whose every hit is skipped shows the no-results state', async () => {
  // The live case: eleven pages with "kernel" on them, none with "kerneltrap".
  hits = [];
  for (let i = 0; i < 11; i++) hits.push(prefixOnlyHit(i));

  await search('Kerneltrap');

  assert.equal(cardCount(), 0);
  assert.equal(el('scolta-no-results').style.display, 'block');
  assert.equal(el('scolta-results-header').innerHTML, '', 'no count over an empty list');
  assert.equal(el('scolta-load-more').style.display, 'none');
});

test('a query of only short words still renders its hits', async () => {
  // "go" is under the three characters a highlight term needs, so the
  // false positive guard had no term to look for and skipped every hit.
  hits = [];
  for (let i = 0; i < 4; i++) {
    hits.push({
      url: `/static/go-${i}.html`,
      excerpt: `how to <mark>go</mark> faster ${i}`,
      content: `Notes ${i}. How to go faster.`,
      meta: { title: `Speed notes${i}x`, url: `/item/go-${i}` },
    });
  }

  await search('go');

  assert.equal(cardCount(), 4, el('scolta-results').innerHTML);
  assert.equal(headerCount(), 4);
});
