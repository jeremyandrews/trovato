// The review images in docs/images/b199/: each page type in light, dark, the
// right-to-left pseudo locale, and 320 px wide, full page. `npm run
// screenshots` against a seeded site; not part of `npm test`.
import { test } from '@playwright/test';
import { setLanguage } from '../specs/helpers.mjs';

const OUT = '../../docs/images/b199';
// SHOTS picks the set: the shell's page types (default), "forms", "lists"
// or "search".
const SETS = {
  shell: {
    dashboard: '/admin',
    landing: '/admin/people/overview',
    list: '/admin/content',
    form: '/admin/structure/stages/add',
    preferences: '/admin/preferences',
    search: '/admin/search?q=content',
  },
  forms: {
    // A hand-written form, a form built in Rust, a settings form with its
    // "What this changes" line, and a failed submit with its error summary.
    'forms-user': '/admin/people/add',
    'forms-item': async (page) => {
      await page.goto('/admin/content/add');
      const href = await page.locator('main a[href^="/admin/content/add/"]').first().getAttribute('href');
      await page.goto(href);
    },
    'forms-settings': '/admin/config/site',
    'forms-errors': async (page) => {
      await page.goto('/admin/people/add');
      await page.locator('#mail').fill('not-an-address');
      await Promise.all([page.waitForLoadState('load'), page.locator('form#user-form [type=submit]').click()]);
    },
  },
  lists: {
    // The listing pattern: a filtered, sorted listing; people; the site
    // log; and the page a Delete asks on.
    'lists-content': '/admin/content?status=1&sort=title',
    'lists-people': '/admin/people',
    'lists-log': '/admin/reports/log',
    'lists-confirm': async (page) => {
      await page.goto('/admin/content');
      await Promise.all([page.waitForURL('**/admin/confirm'), page.locator('tbody tr').first().locator('button.link-button--danger').click()]);
    },
  },
  search: {
    // Admin search: the grouped results page, and the command palette open
    // with results and the first option active.
    'search-results': '/admin/search?q=site',
    'search-palette': async (page) => {
      await page.goto('/admin');
      await page.locator('.admin-palette-open').click();
      await page.keyboard.type('site');
      await page.locator('#admin-palette-results [role=option]').first().waitFor();
      await page.keyboard.press('ArrowDown');
    },
  },
};
const PAGES = SETS[process.env.SHOTS || 'shell'];
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
        if (typeof path === 'function') await path(page);
        else await page.goto(path);
        // A full page capture draws a sticky action bar where the first
        // screen ends, over the form; lay it out in place for the image.
        await page.addStyleTag({ content: '.form-actions--sticky { position: static; }' });
        await page.screenshot({ path: `${OUT}/${type}-${mode}.png`, fullPage: true });
      });
    }
  });
}
