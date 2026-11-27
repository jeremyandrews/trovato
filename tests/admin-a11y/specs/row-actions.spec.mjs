// Row actions in every listing (docs/admin/design.md, "Table pattern"): a
// row action shows its verb alone ("Edit") and is named by the verb and the
// row's name ("Edit About us"), through `aria-labelledby` pointing at itself
// and at the element holding the row's name (WCAG 2.5.3 and 2.4.6).
//
// On every route the inventory opens that shows at least one row action:
// - an action with `aria-labelledby` is named exactly its visible text, one
//   space, and the text of its row's `row-{key}` element;
// - on the listings that keep a full sentence for each action (an item's
//   translations, whose rows are language codes, and its revisions, whose
//   rows are dates), each action is named exactly its visible text;
// - on every other listing, every action carries `aria-labelledby`;
// - an action's row element is in the action's own table row;
// - every id an `aria-labelledby` names is the id of exactly one element;
// - no form sits inside another form, on any page opened. This is read from
//   the HTML as served: a browser drops a nested `<form>` start tag while
//   parsing, so the parsed page never shows one, and the inner form's
//   buttons would quietly post to the outer form's address.
//
// Light mode and the source language only: the names are the same words in
// every mode, and the pseudo locales change only the verbs.
import { test, expect } from '@playwright/test';
import { routes, addressFor, EXPECTED_UNSEEDED, SEEDED_PLUGINS } from '../routes.mjs';

/** Listings whose row actions keep a full sentence for a name. */
const FULL_SENTENCE = [/^\/admin\/content\/[^/]+\/translate$/, /^\/item\/[^/]+\/revisions$/];

const collapse = (s) => s.replace(/\s+/g, ' ').trim();

/** Each `<form` in `html` opened while another was still open. */
export function nestedForms(html) {
  const markup = html
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/<script\b[\s\S]*?<\/script>/gi, '')
    .replace(/<template\b[\s\S]*?<\/template>/gi, '');
  const nested = [];
  let depth = 0;
  for (const m of markup.matchAll(/<(\/?)form\b[^>]*>/gi)) {
    if (m[1]) {
      depth = Math.max(0, depth - 1);
    } else {
      if (depth > 0) nested.push(m[0].slice(0, 160));
      depth += 1;
    }
  }
  return nested;
}

/** Check the row actions on the page `page` shows; returns how many. */
async function checkPage(page, address, response) {
  expect(nestedForms(await response.text()), `a form inside a form on ${address}`).toEqual([]);

  // Every referenced id names exactly one element.
  const badRefs = await page.locator('[aria-labelledby]').evaluateAll((els) =>
    els.flatMap((el) =>
      el
        .getAttribute('aria-labelledby')
        .split(/\s+/)
        .filter(Boolean)
        .filter((id) => document.querySelectorAll(`[id="${CSS.escape(id)}"]`).length !== 1)
        .map((id) => `${id} (${document.querySelectorAll(`[id="${CSS.escape(id)}"]`).length} elements)`),
    ),
  );
  expect(badRefs, `ids named by aria-labelledby on ${address}`).toEqual([]);

  const actions = page.locator('td.operations a, td.operations button');
  const n = await actions.count();
  const fullSentence = FULL_SENTENCE.some((r) => r.test(new URL(address, 'http://x').pathname));
  for (let i = 0; i < n; i += 1) {
    const action = actions.nth(i);
    const facts = await action.evaluate((el) => {
      const ids = (el.getAttribute('aria-labelledby') || '').split(/\s+/).filter(Boolean);
      const row = ids[1] ? document.getElementById(ids[1]) : null;
      return {
        labelledby: ids,
        text: el.textContent,
        rowText: row ? row.textContent : null,
        ownRow: Boolean(row && el.closest('tr') && el.closest('tr').contains(row)),
        html: el.outerHTML.slice(0, 200),
      };
    });
    if (fullSentence) {
      await expect(action, `${facts.html} on ${address}`).toHaveAccessibleName(collapse(facts.text));
      continue;
    }
    expect(facts.labelledby.length, `${facts.html} on ${address} is named by aria-labelledby`).toBe(2);
    expect(facts.labelledby[1], `${facts.html}: its second id names the row`).toMatch(/^row-/);
    expect(facts.rowText, `${facts.html}: its row element`).not.toBeNull();
    expect(facts.ownRow, `${facts.html}: its row element is in its own row`).toBe(true);
    await expect(action, `${facts.html} on ${address}`).toHaveAccessibleName(
      `${collapse(facts.text)} ${collapse(facts.rowText)}`,
    );
  }
  return n;
}

test('a nested form is found in the served HTML, which the parsed page hides', async ({ page }) => {
  const html = '<form action="/a"><div><form action="/b"><button>Go</button></form></div></form>'
    + '<form action="/c"></form><script>const t = "<form>";</script>';
  expect(nestedForms(html)).toEqual(['<form action="/b">']);
  await page.setContent(html);
  expect(await page.locator('form form').count()).toBe(0);
});

test('an action named by another row fails the own-row check', async ({ page }) => {
  const html = `<table><tbody>
    <tr><th><a id="row-a">A</a></th><td class="operations"><a id="act-edit-a" aria-labelledby="act-edit-a row-b" href="#">Edit</a></td></tr>
    <tr><th><a id="row-b">B</a></th><td class="operations"><a id="act-edit-b" aria-labelledby="act-edit-b row-b" href="#">Edit</a></td></tr>
  </tbody></table>`;
  await page.setContent(html);
  await expect(checkPage(page, '/admin/example', { text: async () => html })).rejects.toThrow(/in its own row/);
});

test.describe('row actions', () => {
  test.beforeEach(({}, info) => {
    test.skip(info.project.name !== 'light', 'checked once, in light mode and the source language');
  });

  for (const route of routes) {
    test(`${route.path} (${route.group})`, async ({ page }, info) => {
      const address = await addressFor(page, route);
      test.skip(!address && Boolean(EXPECTED_UNSEEDED.get(route.path)), EXPECTED_UNSEEDED.get(route.path));
      test.skip(!address, `no address for ${route.path}`);
      const response = await page.goto(address);
      const pluginOff = route.plugin && !SEEDED_PLUGINS.has(route.plugin);
      test.skip(pluginOff && response.status() === 404, `${route.plugin} is not installed on this site`);
      expect(response.status(), `${address} opens`).toBe(200);
      const n = await checkPage(page, address, response);
      info.annotations.push({ type: 'row actions', description: `${address}: ${n}` });
    });
  }

  test("an item's revisions keep their full sentence names", async ({ page }) => {
    await page.goto('/admin/content');
    const edit = await page.locator('td.operations a[id^="act-revisions-"]').first().getAttribute('href');
    expect(edit).toMatch(/^\/item\/[^/]+\/revisions$/);
    const response = await page.goto(edit);
    expect(response.status()).toBe(200);
    expect(await checkPage(page, edit, response)).toBeGreaterThan(0);
  });
});
