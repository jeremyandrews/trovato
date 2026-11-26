// The dashboard's panels, "Who can do this?" and the accessibility report
// in a browser. Nothing here changes anything.
import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { useProjectLanguage } from './helpers.mjs';

const TAGS = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22a', 'wcag22aa'];

useProjectLanguage(test);

test('each dashboard panel is a region named by its heading, with its state in words', async ({ page }) => {
  await page.goto('/admin');
  const panels = page.locator('.dashboard-panel');
  const n = await panels.count();
  expect(n, 'the administrator sees the panels').toBeGreaterThanOrEqual(3);
  for (let i = 0; i < n; i += 1) {
    const panel = panels.nth(i);
    const id = await panel.getAttribute('aria-labelledby');
    const heading = page.locator(`#${id}`);
    await expect(heading).toHaveJSProperty('tagName', 'H2');
    await expect(heading).not.toBeEmpty();
    await expect(page.getByRole('region', { name: (await heading.innerText()).trim() })).toHaveCount(1);
    // The state is a sentence of text, not a picture or a colour.
    await expect(panel.locator('.dashboard-panel__summary')).not.toBeEmpty();
    await expect(panel.locator('svg, canvas, img')).toHaveCount(0);
  }
});

test('"Who can do this?" is reached by keyboard after the page and answers for that page', async ({ page }) => {
  await page.goto('/admin/structure/menus');
  const link = page.locator('.who-can-link a');
  await expect(link).toBeVisible();
  // It is the last thing in main: after it, Tab leaves the page content.
  const last = await page.evaluate(() => {
    const focusable = [...document.querySelectorAll('main a[href], main button, main input, main select, main textarea, main [tabindex]:not([tabindex="-1"])')]
      .filter((el) => el.offsetParent !== null);
    return focusable[focusable.length - 1].closest('.who-can-link') !== null;
  });
  expect(last).toBe(true);
  await link.focus();
  await Promise.all([page.waitForURL(/\/admin\/people\/who-can\?path=/), page.keyboard.press('Enter')]);
  await expect(page.locator('h1')).not.toBeEmpty();
  await expect(page.locator('#who-can-needs')).toBeVisible();
  await expect(page.locator('.who-can-link')).toHaveCount(0);
});

test('without a page, "Who can do this?" lists what opens every page', async ({ page }) => {
  await page.goto('/admin/people/who-can');
  const rows = page.locator('table tbody tr');
  expect(await rows.count()).toBeGreaterThan(30);
  await expect(page.locator('table caption')).not.toBeEmpty();
  await expect(rows.first().locator('th[scope=row] a')).toHaveAttribute('href', /who-can\?path=/);
});

test('the accessibility report sits under Reports with a section per topic', async ({ page }) => {
  await page.goto('/admin/reports/overview');
  await Promise.all([page.waitForURL('**/admin/reports/accessibility'), page.locator('main a[href="/admin/reports/accessibility"]').first().click()]);
  for (const id of ['a11y-images', 'a11y-you', 'a11y-checks']) {
    await expect(page.locator(`section[aria-labelledby="${id}"] h2#${id}`)).toHaveCount(1);
  }
  await expect(page.locator('section[aria-labelledby="a11y-you"] a[href="/admin/preferences"]')).toBeVisible();
});

test('the answer for one page has no WCAG 2.2 A or AA violations', async ({ page }) => {
  await page.goto('/admin/people/who-can?path=/admin/structure/menus');
  await expect(page.locator('#who-can-needs')).toBeVisible();
  const results = await new AxeBuilder({ page }).withTags(TAGS).analyze();
  expect(results.violations.map((v) => `${v.id}: ${v.nodes.length}`)).toEqual([]);
});
