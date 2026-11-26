// The command palette (static/js/admin-palette.js) in a browser: an APG
// modal dialog holding an ARIA 1.2 combobox whose listbox groups its
// options, with the same results as /admin/search.
//
// Nothing here changes anything. The shortcut preferences are left at
// their defaults ("/" on, no other key).
import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { useProjectLanguage } from './helpers.mjs';

useProjectLanguage(test);

const TAGS = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22a', 'wcag22aa'];
const QUERY = 'content';

async function openWithButton(page) {
  await page.goto('/admin');
  const button = page.locator('.admin-palette-open');
  await expect(button).toBeVisible();
  await button.focus();
  await page.keyboard.press('Enter');
  await expect(page.locator('#admin-palette')).toBeVisible();
  return button;
}

async function search(page, q = QUERY) {
  await page.keyboard.type(q);
  await expect(page.locator('#admin-palette-results [role=option]').first()).toBeVisible();
}

test('the Search button opens a modal dialog with focus in the combobox', async ({ page }) => {
  const button = await openWithButton(page);
  await expect(button).toHaveAttribute('aria-haspopup', 'dialog');
  const input = page.locator('#admin-palette-q');
  await expect(input).toBeFocused();
  await expect(input).toHaveAttribute('role', 'combobox');
  await expect(input).toHaveAttribute('aria-expanded', 'false');
  await expect(input).toHaveAttribute('aria-controls', 'admin-palette-results');
  // The dialog is modal: the rest of the page is out of reach.
  expect(await page.evaluate(() => document.querySelector('#admin-palette').matches(':modal'))).toBe(true);
  await expect(page.getByRole('dialog')).toHaveCount(1);
});

test('results are options grouped under headings, and the count is announced', async ({ page }) => {
  await openWithButton(page);
  await search(page);
  const input = page.locator('#admin-palette-q');
  await expect(input).toHaveAttribute('aria-expanded', 'true');
  const listbox = page.locator('#admin-palette-results');
  await expect(listbox).toHaveAttribute('role', 'listbox');
  const groups = listbox.locator('[role=group]');
  expect(await groups.count()).toBeGreaterThan(0);
  for (let i = 0; i < await groups.count(); i += 1) {
    const labelledby = await groups.nth(i).getAttribute('aria-labelledby');
    await expect(page.locator(`#${labelledby}`)).not.toBeEmpty();
  }
  const options = listbox.locator('[role=option]');
  const n = await options.count();
  const status = page.locator('#admin-palette-count');
  await expect(status).toHaveAttribute('role', 'status');
  await expect(status).toContainText(String(n));
});

test('Up and Down move the active option, which the combobox names', async ({ page }) => {
  await openWithButton(page);
  await search(page);
  const input = page.locator('#admin-palette-q');
  const options = page.locator('#admin-palette-results [role=option]');
  const n = await options.count();
  await page.keyboard.press('ArrowDown');
  const first = await options.nth(0).getAttribute('id');
  await expect(input).toHaveAttribute('aria-activedescendant', first);
  await expect(options.nth(0)).toHaveAttribute('aria-selected', 'true');
  if (n > 1) {
    await page.keyboard.press('ArrowDown');
    await expect(input).toHaveAttribute('aria-activedescendant', await options.nth(1).getAttribute('id'));
    await expect(options.nth(0)).toHaveAttribute('aria-selected', 'false');
    await page.keyboard.press('ArrowUp');
  }
  // Up from the first wraps to the last.
  await page.keyboard.press('ArrowUp');
  await expect(input).toHaveAttribute('aria-activedescendant', await options.nth(n - 1).getAttribute('id'));
  // Focus never left the input.
  await expect(input).toBeFocused();
});

test('the palette lists the same results as the search page', async ({ page }) => {
  await openWithButton(page);
  await search(page);
  const suggested = await page.locator('#admin-palette-results [role=option]').evaluateAll(
    (os) => os.map((o) => o.getAttribute('data-href')),
  );
  await page.goto(`/admin/search?q=${QUERY}`);
  const listed = await page.locator('.search-results .landing-list__link').evaluateAll(
    (as) => as.map((a) => a.getAttribute('href')),
  );
  expect(suggested).toEqual(listed);
});

test('Enter opens the active option', async ({ page }) => {
  await openWithButton(page);
  await search(page);
  await page.keyboard.press('ArrowDown');
  const href = await page.locator('#admin-palette-results [role=option]').first().getAttribute('data-href');
  await Promise.all([page.waitForURL((u) => `${u.pathname}${u.search}${u.hash}` === href), page.keyboard.press('Enter')]);
});

test('Enter with no active option opens the full results page', async ({ page }) => {
  await openWithButton(page);
  await search(page);
  await Promise.all([page.waitForURL(/\/admin\/search\?q=content/), page.keyboard.press('Enter')]);
});

test('Escape closes the palette and focus returns to the button', async ({ page }) => {
  const button = await openWithButton(page);
  await search(page);
  await page.keyboard.press('Escape');
  await expect(page.locator('#admin-palette')).toBeHidden();
  await expect(button).toBeFocused();
});

test('/ opens the palette, except while typing in a text field', async ({ page }) => {
  await page.goto('/admin');
  await expect(page.locator('.admin-palette-open')).toHaveAttribute('aria-keyshortcuts', '/');
  // In a text field "/" is a character.
  await page.locator('#admin-search-q').focus();
  await page.keyboard.press('/');
  await expect(page.locator('#admin-palette')).toBeHidden();
  await expect(page.locator('#admin-search-q')).toHaveValue('/');
  // Anywhere else it opens the palette, and Escape returns focus.
  // A link in the banner: shown at every width.
  const link = page.locator('.admin-account a').first();
  await link.focus();
  await page.keyboard.press('/');
  await expect(page.locator('#admin-palette')).toBeVisible();
  await expect(page.locator('#admin-palette-q')).toBeFocused();
  await expect(page.locator('#admin-palette-q')).toHaveValue('');
  await page.keyboard.press('Escape');
  await expect(link).toBeFocused();
});

test('the open palette with results has no WCAG 2.2 A or AA violations', async ({ page }) => {
  await openWithButton(page);
  await search(page);
  await page.keyboard.press('ArrowDown');
  const axe = await new AxeBuilder({ page }).include('#admin-palette').withTags(TAGS).analyze();
  expect(axe.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`)).toEqual([]);
});

test.describe('with JavaScript off', () => {
  test.use({ javaScriptEnabled: false });

  test('there is no palette button, and the search form leads to the same results', async ({ page }) => {
    await page.goto('/admin');
    await expect(page.locator('.admin-palette-open')).toBeHidden();
    await expect(page.locator('#admin-palette')).toBeHidden();
    await page.locator('#admin-search-q').fill(QUERY);
    await Promise.all([page.waitForURL(/\/admin\/search\?q=content/), page.locator('#admin-search-q').press('Enter')]);
    await expect(page.locator('.search-results').first()).toBeVisible();
  });
});
