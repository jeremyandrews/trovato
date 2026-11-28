// What a screen reader is given on each page type: landmarks, the heading,
// the breadcrumb, the sidebar. The full tree of each page type is written
// to test-results/aria/, or with ADMIN_A11Y_RECORD_ARIA=1 to
// docs/admin/screen-reader/, which docs/admin/accessibility.md reads.
import { test, expect } from '@playwright/test';
import fs from 'node:fs';
import { useProjectLanguage } from './helpers.mjs';

useProjectLanguage(test);

const PAGE_TYPES = {
  dashboard: '/admin',
  landing: '/admin/people/overview',
  // Landing pages with status lines and a main action (B245), and the
  // sidebar on a page three levels down.
  'landing-content': '/admin/content/overview',
  'landing-structure': '/admin/structure/overview',
  'landing-reports': '/admin/reports/overview',
  'deep-page': async (page) => {
    await page.goto('/admin/people/roles');
    await page.goto(await page.locator('main a[href^="/admin/people/roles/"][href$="/edit"]').first().getAttribute('href'));
  },
  list: '/admin/content',
  form: '/admin/structure/stages/add',
  preferences: '/admin/preferences',
  search: '/admin/search?q=content',
  // The form pattern: a hand-written form, one built in Rust, a settings
  // form, and a failed submit.
  'form-user': '/admin/people/add',
  'form-item': async (page) => {
    await page.goto('/admin/content/add');
    await page.goto(await page.locator('main a[href^="/admin/content/add/"]').first().getAttribute('href'));
  },
  'form-settings': '/admin/config/site',
  'form-errors': async (page) => {
    await page.goto('/admin/people/roles/add');
    await Promise.all([page.waitForLoadState('load'), page.locator('form#role_form [type=submit]').click()]);
  },
  // The listing pattern: filtered and sorted, and the confirmation page a
  // Delete leads to.
  'list-filtered': '/admin/content?status=1&sort=title',
  // The dashboard's panels are in the dashboard tree; who can open a page,
  // and the accessibility report.
  'who-can': '/admin/people/who-can?path=/admin/structure/menus',
  'a11y-report': '/admin/reports/accessibility',
  confirm: async (page) => {
    // A row's Delete opens its own page; a bulk delete still asks here.
    await page.goto('/admin/content');
    await page.locator('[data-select-row]').first().check();
    await page.selectOption('#content_bulk_form #action', 'delete');
    await Promise.all([page.waitForURL('**/admin/confirm'), page.locator('#content_bulk_form [type=submit]').click()]);
  },
};

for (const [type, path] of Object.entries(PAGE_TYPES)) {
  test(`the ${type} page exposes the shell's landmarks and one heading`, async ({ page }, info) => {
    test.skip(info.project.name !== 'light', 'the tree is the same in every colour mode');
    if (typeof path === 'function') await path(page);
    else await page.goto(path);
    const body = page.locator('body');
    await expect(body).toMatchAriaSnapshot(`
      - link /Skip to main content/
      - banner:
        - search
      - navigation "Administration"
      - main
    `);
    await expect(page.locator('main h1')).toHaveCount(1);
    await expect(page.getByRole('heading', { level: 1 })).toHaveCount(1);
    if (type !== 'dashboard') {
      const crumbs = page.getByRole('navigation', { name: 'Breadcrumb' });
      await expect(crumbs).toBeVisible();
      await expect(crumbs.locator('[aria-current="page"]')).toHaveText(await page.locator('h1').innerText());
    }
    if (type === 'form-errors') {
      // The summary is an alert that has focus, with a link per problem;
      // the failed field is invalid and its message is its description.
      await expect(page.getByRole('alert').filter({ hasText: /./ }).first()).toBeFocused();
      await expect(page.locator('[data-error-summary] a[href="#name"]')).toHaveCount(1);
      await expect(page.locator('#name')).toHaveAttribute('aria-invalid', 'true');
    }
    const tree = await body.ariaSnapshot();
    // ADMIN_A11Y_RECORD_ARIA=1 writes them where docs/admin/accessibility.md
    // reads them.
    const dir = process.env.ADMIN_A11Y_RECORD_ARIA ? '../../docs/admin/screen-reader' : 'test-results/aria';
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(`${dir}/${type}.yml`, tree + '\n');
  });
}

// The command palette open with results: a modal dialog, so its tree is
// the dialog's own. The combobox names its listbox and the active option.
test('the open command palette is a dialog with a combobox and a grouped listbox', async ({ page }, info) => {
  test.skip(info.project.name !== 'light', 'the tree is the same in every colour mode');
  await page.goto('/admin');
  await page.locator('.admin-palette-open').click();
  await page.keyboard.type('content');
  await expect(page.locator('#admin-palette-results [role=option]').first()).toBeVisible();
  await page.keyboard.press('ArrowDown');
  const dialog = page.getByRole('dialog');
  await expect(dialog).toMatchAriaSnapshot(`
    - dialog:
      - search:
        - heading [level=2]
        - combobox [expanded]
        - listbox:
          - group:
            - option [selected]
  `);
  const tree = await dialog.ariaSnapshot();
  const dir = process.env.ADMIN_A11Y_RECORD_ARIA ? '../../docs/admin/screen-reader' : 'test-results/aria';
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(`${dir}/palette.yml`, tree + '\n');
});
