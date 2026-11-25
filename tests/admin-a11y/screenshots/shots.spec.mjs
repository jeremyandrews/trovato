// The review images in docs/images/b199/: each page type in light, dark, the
// right-to-left pseudo locale, and 320 px wide, full page. `npm run
// screenshots` against a seeded site; not part of `npm test`.
import { test } from '@playwright/test';
import { setLanguage } from '../specs/helpers.mjs';

const OUT = '../../docs/images/b199';
const PAGES = {
  dashboard: '/admin',
  landing: '/admin/people/overview',
  list: '/admin/content',
  form: '/admin/structure/stages/add',
  preferences: '/admin/preferences',
  search: '/admin/search?q=content',
};
const MODES = [
  { mode: 'light', width: 1280, scheme: 'light' },
  { mode: 'dark', width: 1280, scheme: 'dark' },
  { mode: 'narrow', width: 320, scheme: 'light' },
  { mode: 'rtl', width: 1280, scheme: 'light', language: 'ar-XB' },
];

for (const { mode, width, scheme, language } of MODES) {
  test.describe(mode, () => {
    test.use({ viewport: { width, height: 900 }, colorScheme: scheme });
    if (language) {
      test.beforeAll(async ({ browser }, info) => {
        const page = await browser.newPage({ storageState: '.auth/admin.json', baseURL: info.project.use.baseURL });
        await setLanguage(page, language);
        await page.close();
      });
      test.afterAll(async ({ browser }, info) => {
        const page = await browser.newPage({ storageState: '.auth/admin.json', baseURL: info.project.use.baseURL });
        await setLanguage(page, 'en');
        await page.close();
      });
    }
    for (const [type, path] of Object.entries(PAGES)) {
      test(`${type} ${mode}`, async ({ page }) => {
        await page.goto(path);
        await page.screenshot({ path: `${OUT}/${type}-${mode}.png`, fullPage: true });
      });
    }
  });
}
