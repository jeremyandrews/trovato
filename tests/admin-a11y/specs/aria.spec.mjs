// What a screen reader is given on each page type: landmarks, the heading,
// the breadcrumb, the sidebar. The full tree of each page type is written
// to test-results/aria/ for review (docs/admin/accessibility.md quotes them).
import { test, expect } from '@playwright/test';
import fs from 'node:fs';
import { useProjectLanguage } from './helpers.mjs';

useProjectLanguage(test);

const PAGE_TYPES = {
  dashboard: '/admin',
  landing: '/admin/people/overview',
  list: '/admin/content',
  form: '/admin/structure/stages/add',
  preferences: '/admin/preferences',
  search: '/admin/search?q=content',
};

for (const [type, path] of Object.entries(PAGE_TYPES)) {
  test(`the ${type} page exposes the shell's landmarks and one heading`, async ({ page }, info) => {
    test.skip(info.project.name !== 'light', 'the tree is the same in every colour mode');
    await page.goto(path);
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
    const tree = await body.ariaSnapshot();
    fs.mkdirSync('test-results/aria', { recursive: true });
    fs.writeFileSync(`test-results/aria/${type}.yml`, tree + '\n');
  });
}
