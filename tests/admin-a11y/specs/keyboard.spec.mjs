// The shell by keyboard alone: the skip link, the sidebar disclosures, the
// narrow-screen menu, and the search shortcut-free path (Tab to the field).
import { test, expect } from '@playwright/test';
import { useProjectLanguage } from './helpers.mjs';

useProjectLanguage(test);

test('the first Tab reaches the skip link, which moves focus to the main content', async ({ page }) => {
  await page.goto('/admin/content');
  await page.keyboard.press('Tab');
  const skip = page.locator('a.skip-link');
  await expect(skip).toBeFocused();
  await expect(skip).toBeVisible();
  await page.keyboard.press('Enter');
  await expect(page.locator('main#main')).toBeFocused();
});

test('a sidebar group opens and closes with Enter and Space, and stays that way on the next page', async ({ page }, info) => {
  test.skip(info.project.name === 'narrow', 'the sidebar is behind the menu button at this width');
  await page.goto('/admin/content');
  const toggle = page.locator('button.admin-nav__toggle[aria-controls="admin-nav-reports"]');
  await expect(toggle).toBeVisible();
  const before = await toggle.getAttribute('aria-expanded');
  await toggle.focus();
  const saved = page.waitForResponse((r) => r.url().endsWith('/admin/preferences/nav'));
  await page.keyboard.press('Enter');
  await saved;
  const after = before === 'true' ? 'false' : 'true';
  await expect(toggle).toHaveAttribute('aria-expanded', after);
  await expect(page.locator('#admin-nav-reports')).toBeVisible({ visible: after === 'true' });

  await page.goto('/admin/content');
  await expect(page.locator('button[aria-controls="admin-nav-reports"]')).toHaveAttribute('aria-expanded', after);

  // Put it back with Space.
  const restored = page.waitForResponse((r) => r.url().endsWith('/admin/preferences/nav'));
  await page.locator('button[aria-controls="admin-nav-reports"]').focus();
  await page.keyboard.press('Space');
  await restored;
  await expect(page.locator('button[aria-controls="admin-nav-reports"]')).toHaveAttribute('aria-expanded', before);
});

test('a group opened and closed quickly is remembered as last left, even when the first save is slow', async ({ page }, info) => {
  test.skip(info.project.name === 'narrow', 'the sidebar is behind the menu button at this width');
  await page.goto('/admin/content');
  const toggle = page.locator('button[aria-controls="admin-nav-appearance"]');
  const before = await toggle.getAttribute('aria-expanded');
  // Hold the first save back, so a second save sent at once would be stored first.
  let held = false;
  await page.route('**/admin/preferences/nav', async (route) => {
    if (!held) {
      held = true;
      await new Promise((resolve) => setTimeout(resolve, 800));
    }
    await route.continue();
  });
  let answered = 0;
  page.on('response', (r) => { if (r.url().endsWith('/admin/preferences/nav')) answered += 1; });
  await toggle.click();
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-expanded', before);
  await expect.poll(() => answered, { timeout: 10000 }).toBe(2);
  await page.unroute('**/admin/preferences/nav');
  await page.goto('/admin/content');
  await expect(page.locator('button[aria-controls="admin-nav-appearance"]')).toHaveAttribute('aria-expanded', before);
});

test('closing the current page\'s group is not saved: it opens again on the next page', async ({ page }, info) => {
  test.skip(info.project.name === 'narrow', 'the sidebar is behind the menu button at this width');
  await page.goto('/admin/people/roles');
  const toggle = page.locator('button[aria-controls="admin-nav-people"]');
  await expect(toggle).toHaveAttribute('aria-expanded', 'true');
  let posted = false;
  page.on('request', (r) => { if (r.url().endsWith('/admin/preferences/nav')) posted = true; });
  await toggle.focus();
  await page.keyboard.press('Enter');
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  await page.goto('/admin/people/roles');
  expect(posted, 'the current group\'s state was posted').toBe(false);
  await expect(page.locator('button[aria-controls="admin-nav-people"]')).toHaveAttribute('aria-expanded', 'true');
});

test('the group holding the current page is open', async ({ page }) => {
  await page.goto('/admin/people/roles');
  const current = page.locator('#admin-nav a[aria-current="page"]');
  await expect(current).toHaveCount(1);
  await expect(current).toHaveAttribute('href', '/admin/people/roles');
});

test('on a narrow screen the menu button opens the sidebar, and Escape closes it and returns focus', async ({ page }, info) => {
  test.skip(info.project.name !== 'narrow', 'narrow screens only');
  await page.goto('/admin/content');
  const menu = page.locator('button.admin-menu-toggle');
  await expect(menu).toBeVisible();
  await expect(page.locator('#admin-nav')).toBeHidden();
  await menu.focus();
  await page.keyboard.press('Enter');
  await expect(menu).toHaveAttribute('aria-expanded', 'true');
  await expect(page.locator('#admin-nav')).toBeVisible();
  // The disclosure does not move focus: the opened sidebar is the next
  // region in the tab order after the search field and account links.
  let inNav = false;
  for (let i = 0; i < 8 && !inNav; i++) {
    await page.keyboard.press('Tab');
    inNav = await page.evaluate(() => !!document.activeElement.closest('#admin-nav'));
  }
  expect(inNav, 'the opened sidebar is reachable by Tab').toBe(true);
  await page.keyboard.press('Escape');
  await expect(menu).toHaveAttribute('aria-expanded', 'false');
  await expect(menu).toBeFocused();
});

test('search is reachable by Tab and answers on Enter', async ({ page }) => {
  await page.goto('/admin');
  const field = page.locator('#admin-search-q');
  for (let i = 0; i < 12 && !(await field.evaluate((el) => el === document.activeElement)); i++) {
    await page.keyboard.press('Tab');
  }
  await expect(field).toBeFocused();
  await page.keyboard.type('roles');
  await Promise.all([page.waitForURL('**/admin/search?q=roles'), page.keyboard.press('Enter')]);
  await expect(page.locator('main a[href="/admin/people/roles"]')).toBeVisible();
});

test('every focusable element in the shell shows a focus indicator', async ({ page }, info) => {
  test.skip(info.project.name === 'narrow', 'checked at desktop width');
  await page.goto('/admin/content');
  const missing = [];
  for (let i = 0; i < 25; i++) {
    await page.keyboard.press('Tab');
    const info2 = await page.evaluate(() => {
      const el = document.activeElement;
      if (!el || el === document.body) return null;
      const s = getComputedStyle(el);
      const ring = parseFloat(s.outlineWidth) >= 2 && s.outlineStyle !== 'none';
      return { ring, label: (el.textContent || el.getAttribute('aria-label') || el.tagName).trim().slice(0, 40) };
    });
    if (info2 && !info2.ring) missing.push(info2.label);
  }
  expect(missing, 'focused without a visible ring of at least 2px').toEqual([]);
});
