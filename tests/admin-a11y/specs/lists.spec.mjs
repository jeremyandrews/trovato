// The admin listing pattern in a browser (docs/admin/design.md section 9):
// - select all and the live selected count;
// - sort buttons in column headers that carry aria-sort;
// - the result count announced after a filter;
// - a Delete that asks on its own page with JavaScript off;
// - the narrow layout that needs no sideways scroll.
//
// Nothing here deletes or changes anything: each confirmation is cancelled.
import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { useProjectLanguage, expectNoPageScroll } from './helpers.mjs';

useProjectLanguage(test);

const TAGS = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22a', 'wcag22aa'];

test('select all checks every row, shows mixed for some, and the count is announced', async ({ page }) => {
  await page.goto('/admin/content');
  const all = page.locator('[data-select-all]');
  const rows = page.locator('[data-select-row]');
  const count = page.locator('[data-selected-count]');
  const n = await rows.count();
  test.skip(n < 2, 'the seeded site lists fewer than two items');
  await expect(count).toHaveAttribute('role', 'status');

  await all.focus();
  await page.keyboard.press('Space');
  for (let i = 0; i < n; i += 1) await expect(rows.nth(i)).toBeChecked();
  await expect(count).toContainText(String(n));

  await rows.first().focus();
  await page.keyboard.press('Space');
  expect(await all.evaluate((el) => el.indeterminate)).toBe(true);
  await expect(count).toContainText(String(n - 1));
});

test('a sort button sorts by its column and the header says so', async ({ page }) => {
  await page.goto('/admin/content');
  const button = page.locator('th button[value="title"]');
  await button.focus();
  await Promise.all([page.waitForURL(/sort=title/), page.keyboard.press('Enter')]);
  const th = page.locator('th[aria-sort]');
  await expect(th).toHaveCount(1);
  await expect(th).toHaveAttribute('aria-sort', 'ascending');
  await expect(th.locator('button')).toHaveAttribute('value', '-title');
});

test('after a filter the result count is announced in a live region', async ({ page }) => {
  await page.goto('/admin/content');
  const first = (await page.locator('tbody th[scope=row] a').first().textContent()).trim();
  const word = first.split(/\s+/)[0];
  await page.locator('#q').fill(word);
  await Promise.all([page.waitForURL(/[?&]q=/), page.locator('#list-filter [type=submit]').click()]);
  const live = page.locator('#list-live');
  await expect(live).toHaveAttribute('role', 'status');
  await expect(live).not.toBeEmpty();
  await expect(live).toHaveText((await page.locator('[data-list-count]').textContent()).trim());
});

test.describe('with JavaScript off', () => {
  test.use({ javaScriptEnabled: false });

  test('Delete asks on its own page, which names what it deletes, and Cancel keeps it', async ({ page }) => {
    await page.goto('/admin/content');
    const row = page.locator('tbody tr').first();
    const title = (await row.locator('th[scope=row]').textContent()).trim();
    await Promise.all([page.waitForURL('**/admin/content/*/delete'), row.locator('a.link-button--danger').click()]);
    await expect(page.locator('h1')).toContainText(title);
    await expect(page.locator('main .admin-card > p').first()).toContainText(title);
    await expect(page.locator('#content_delete_form .button--danger')).toBeVisible();
    await Promise.all([page.waitForURL('**/admin/content'), page.locator('main form .button--secondary').click()]);
    await expect(page.locator('tbody th[scope=row]', { hasText: title }).first()).toBeVisible();
  });

  test('a bulk delete names how many items before it deletes', async ({ page }) => {
    await page.goto('/admin/content');
    const rows = page.locator('[data-select-row]');
    test.skip((await rows.count()) < 2, 'the seeded site lists fewer than two items');
    await rows.nth(0).check();
    await rows.nth(1).check();
    await page.selectOption('#content_bulk_form #action', 'delete');
    await Promise.all([page.waitForURL('**/admin/confirm'), page.locator('#content_bulk_form [type=submit]').click()]);
    await expect(page.locator('.confirm__question')).toContainText('2');
    await expect(page.locator('main form .button--danger')).toContainText('2');
    await Promise.all([page.waitForURL('**/admin/**'), page.locator('main form .button--secondary').click()]);
  });
});

test('on a narrow screen a listing lays rows out as blocks, with no sideways scroll', async ({ page }, info) => {
  test.skip(info.project.name !== 'narrow', 'the narrow layout');
  await page.goto('/admin/content');
  await expect(page.locator('table.table--stacked')).toHaveCount(1);
  await expectNoPageScroll(page);
  const region = page.locator('.table-scroll').first();
  const sideways = await region.evaluate((el) => el.scrollWidth - el.clientWidth);
  expect(sideways, 'the table scrolls sideways').toBeLessThanOrEqual(1);
  // Each cell says which column it is.
  await expect(page.locator('tbody td[data-label]').first()).toBeVisible();
});

test('the confirmation page has no WCAG 2.2 A or AA violations', async ({ page }) => {
  // With scripts on, a bulk delete leads to the same page: nothing asks in a
  // script.
  await page.goto('/admin/content');
  await page.locator('[data-select-row]').first().check();
  await page.selectOption('#content_bulk_form #action', 'delete');
  await Promise.all([page.waitForURL('**/admin/confirm'), page.locator('#content_bulk_form [type=submit]').click()]);
  const axe = await new AxeBuilder({ page }).withTags(TAGS).analyze();
  expect(axe.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`)).toEqual([]);
  await Promise.all([page.waitForURL('**/admin/content**'), page.locator('main form .button--secondary').click()]);
});
