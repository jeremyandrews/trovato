// The admin form pattern in a browser (docs/admin/design.md section 8): a
// failed submit leads with an error summary that takes focus and links to
// each field; the page has no axe violations in that state; a long form's
// action bar never hides the focused field; the unsaved changes warning
// appears only while a form is dirty; native details open from the keyboard;
// the media picker is a modal dialog.
//
// Nothing here saves anything: every submit is one the server refuses.
import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { useProjectLanguage } from './helpers.mjs';

useProjectLanguage(test);

const TAGS = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22a', 'wcag22aa'];

/** Submit the Add role form empty: refused, nothing created. */
async function failRoleForm(page) {
  await page.goto('/admin/people/roles/add');
  await page.locator('#name').fill('');
  await Promise.all([page.waitForLoadState('load'), page.locator('form#role-form [type=submit]').click()]);
}

test('a failed submit leads with an error summary that takes focus and links to its field', async ({ page }) => {
  await failRoleForm(page);
  const summary = page.locator('[data-error-summary]');
  await expect(summary).toBeVisible();
  await expect(summary).toBeFocused();
  await expect(summary).toHaveAttribute('role', 'alert');
  expect(await page.title()).toMatch(/^\S+:? /);
  expect(await page.title()).toContain(await summary.getAttribute('data-error-summary'));

  const field = page.locator('#name');
  await expect(field).toHaveAttribute('aria-invalid', 'true');
  const described = (await field.getAttribute('aria-describedby')).split(' ');
  expect(described).toContain('name-error');
  await expect(page.locator('#name-error')).toBeVisible();

  // Tab to the summary's link and follow it: focus lands on the field.
  await page.keyboard.press('Tab');
  await expect(summary.locator('a[href="#name"]')).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(field).toBeFocused();
});

test('a form in its error state has no WCAG 2.2 A or AA violations', async ({ page }) => {
  await failRoleForm(page);
  const results = await new AxeBuilder({ page }).withTags(TAGS).analyze();
  expect(results.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`)).toEqual([]);
});

test('on a long form the action bar stays in view and never covers the focused field', async ({ page }, info) => {
  test.skip(info.project.name === 'narrow', 'the short window below stands in for the narrow case');
  await page.setViewportSize({ width: 1280, height: 420 });
  await page.goto('/admin/people/add');
  const bar = page.locator('form#user-form .form-actions');
  await expect(bar).toHaveClass(/form-actions--sticky/);
  const fields = page.locator('form#user-form :is(input:not([type=hidden]), select, textarea)');
  const count = await fields.count();
  expect(count).toBeGreaterThan(3);
  await page.locator('#name').focus();
  for (let i = 0; i < count; i += 1) {
    const focused = page.locator(':focus');
    const box = await focused.boundingBox();
    const barBox = await bar.boundingBox();
    if (box && barBox) {
      expect(box.y + box.height, `the focused field ${i} is hidden behind the action bar`).toBeLessThanOrEqual(barBox.y + 1);
    }
    await page.keyboard.press('Tab');
  }
});

test('the unsaved changes warning appears only while the form differs from what was loaded', async ({ page }) => {
  await page.goto('/admin/people/roles/add');
  let asked = 0;
  page.on('dialog', async (dialog) => {
    if (dialog.type() === 'beforeunload') asked += 1;
    await dialog.accept();
  });
  // Clean: leaving asks nothing.
  await page.goto('/admin/people/roles');
  expect(asked).toBe(0);

  // Dirty: leaving asks.
  await page.goto('/admin/people/roles/add');
  await page.locator('#name').click();
  await page.keyboard.type('Unsaved');
  await page.goto('/admin/people/roles');
  expect(asked).toBe(1);

  // Typed and then undone is clean again.
  await page.goto('/admin/people/roles/add');
  await page.locator('#name').click();
  await page.keyboard.type('x');
  await page.keyboard.press('Backspace');
  await page.goto('/admin/people/roles');
  expect(asked).toBe(1);
});

test('collapsed settings are native details that open from the keyboard', async ({ page }) => {
  // The first admin form page that collapses advanced settings.
  const candidates = ['/admin/config/site', '/admin/system/ai-assistant', '/admin/structure/types/add', '/admin/config/search'];
  let found = false;
  for (const path of candidates) {
    const response = await page.goto(path);
    if (!response || !response.ok()) continue;
    const summary = page.locator('details:not([open]) > summary').first();
    if ((await summary.count()) === 0) continue;
    found = true;
    // Pin this one: the selector above matches the next closed one once
    // this opens.
    const handle = await summary.elementHandle();
    const open = () => handle.evaluate((el) => el.parentElement.open);
    await handle.focus();
    expect(await handle.evaluate((el) => el === document.activeElement)).toBe(true);
    await page.keyboard.press('Enter');
    await expect.poll(open).toBe(true);
    await page.keyboard.press('Space');
    await expect.poll(open).toBe(false);
    break;
  }
  expect(found, `no collapsed details on ${candidates.join(', ')}`).toBe(true);
});

test('the media picker is a modal dialog: labelled, Escape closes it, focus returns', async ({ page }) => {
  // Any admin form with a file field offers "Browse media".
  await page.goto('/admin/content/add');
  const links = await page.locator('main a[href^="/admin/content/add/"]').evaluateAll((as) => as.map((a) => a.getAttribute('href')));
  let opener = null;
  for (const href of links) {
    await page.goto(href);
    const button = page.locator('button.media-picker-browse').first();
    if ((await button.count()) > 0) {
      opener = button;
      break;
    }
  }
  test.skip(!opener, 'no content type on this site has a file field');
  await opener.focus();
  await page.keyboard.press('Enter');
  const dialog = page.locator('dialog.admin-picker');
  await expect(dialog).toBeVisible();
  await expect(dialog).toHaveAttribute('aria-labelledby', 'admin-picker-title');
  await expect(page.locator('label[for="admin-picker-type"]')).toBeVisible();
  await expect(page.locator('label[for="admin-picker-q"]')).toBeVisible();
  await expect(page.locator('label[for="admin-picker-upload"]')).toBeVisible();
  const axe = await new AxeBuilder({ page }).include('dialog.admin-picker').withTags(TAGS).analyze();
  expect(axe.violations.map((v) => v.id)).toEqual([]);
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
  await expect(opener).toBeFocused();
});
