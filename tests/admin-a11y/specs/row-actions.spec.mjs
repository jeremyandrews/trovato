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
// - every id an `aria-labelledby` names is the id of exactly one element;
// - no form sits inside another form, on any page opened.
//
// Light mode and the source language only: the names are the same words in
// every mode, and the pseudo locales change only the verbs.
import { test, expect } from '@playwright/test';
import { routes, addressFor, EXPECTED_UNSEEDED, SEEDED_PLUGINS } from '../routes.mjs';

/** Listings whose row actions keep a full sentence for a name. */
const FULL_SENTENCE = [/^\/admin\/content\/[^/]+\/translate$/, /^\/item\/[^/]+\/revisions$/];

const collapse = (s) => s.replace(/\s+/g, ' ').trim();

/** Check the row actions on the page `page` shows; returns how many. */
async function checkPage(page, address) {
  expect(await page.locator('form form').count(), `a form inside a form on ${address}`).toBe(0);

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
    await expect(action, `${facts.html} on ${address}`).toHaveAccessibleName(
      `${collapse(facts.text)} ${collapse(facts.rowText)}`,
    );
  }
  return n;
}

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
      const n = await checkPage(page, address);
      info.annotations.push({ type: 'row actions', description: `${address}: ${n}` });
    });
  }

  test("an item's revisions keep their full sentence names", async ({ page }) => {
    await page.goto('/admin/content');
    const edit = await page.locator('td.operations a[id^="act-revisions-"]').first().getAttribute('href');
    expect(edit).toMatch(/^\/item\/[^/]+\/revisions$/);
    const response = await page.goto(edit);
    expect(response.status()).toBe(200);
    expect(await checkPage(page, edit)).toBeGreaterThan(0);
  });
});
