// The review images: each set writes to its own directory under docs/images/
// (b199's sets to docs/images/b199/, b245's to docs/images/b245/). A shot is
// taken in light, dark, the right-to-left pseudo locale and 320 px wide unless
// it names its own modes, full page unless it says otherwise. `npm run
// screenshots` against a seeded site; not part of `npm test`.
import { test } from '@playwright/test';
import { setLanguage } from '../specs/helpers.mjs';

const B199 = '../../docs/images/b199';
const B245 = '../../docs/images/b245';
// SHOTS picks the set: the shell's page types (default), "forms", "lists",
// "search", "dashboard" or "b245". Each set is { out, shots }; a shot is a
// path, a function that opens the page, or { open, modes, fullPage }.
const SETS = {
  shell: { out: B199, shots: {
    dashboard: '/admin',
    landing: '/admin/people/overview',
    list: '/admin/content',
    form: '/admin/structure/stages/add',
    preferences: '/admin/preferences',
    search: '/admin/search?q=content',
  } },
  forms: { out: B199, shots: {
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
      await Promise.all([page.waitForLoadState('load'), page.locator('form#user_form [type=submit]').click()]);
    },
  } },
  lists: { out: B199, shots: {
    // The listing pattern: a filtered, sorted listing; people; the site
    // log; and the page a Delete asks on.
    'lists-content': '/admin/content?status=1&sort=title',
    'lists-people': '/admin/people',
    'lists-log': '/admin/reports/log',
    'lists-confirm': async (page) => {
      await page.goto('/admin/content');
      await page.locator('[data-select-row]').first().check();
      await page.selectOption('#content_bulk_form #action', 'delete');
      await Promise.all([page.waitForURL('**/admin/confirm'), page.locator('#content_bulk_form [type=submit]').click()]);
    },
  } },
  dashboard: { out: B199, shots: {
    // The dashboard's panels, who can open a page, the accessibility report.
    'dashboard-panels': '/admin',
    'dashboard-who-can': '/admin/people/who-can?path=/admin/structure/menus',
    'dashboard-a11y-report': '/admin/reports/accessibility',
  } },
  search: { out: B199, shots: {
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
  } },
  b245: { out: B245, shots: {
    // Wayfinding: every group's landing page and the sidebar on a deep page
    // in light; the Dashboard and the Structure landing page in every mode
    // (the first screen); and the whole Dashboard.
    'landing-content': { open: '/admin/content/overview', modes: ['light'] },
    'landing-structure': { open: '/admin/structure/overview', fullPage: false },
    'landing-appearance': { open: '/admin/appearance/overview', modes: ['light'] },
    'landing-people': { open: '/admin/people/overview', modes: ['light'] },
    'landing-extend': { open: '/admin/extend/overview', modes: ['light'] },
    'landing-config': { open: '/admin/config/overview', modes: ['light'] },
    'landing-reports': { open: '/admin/reports/overview', modes: ['light'] },
    'sidebar-deep': {
      open: async (page) => {
        await page.goto('/admin/people/roles');
        const href = await page.locator('main a[href^="/admin/people/roles/"][href$="/edit"]').first().getAttribute('href');
        await page.goto(href);
      },
      modes: ['light'],
      fullPage: false,
    },
    dashboard: { open: '/admin', fullPage: false },
    'dashboard-full': { open: '/admin', modes: ['light'] },
  } },
};
const SET = SETS[process.env.SHOTS || 'shell'];
const MODES = [
  { mode: 'light', width: 1280, scheme: 'light' },
  { mode: 'dark', width: 1280, scheme: 'dark' },
  { mode: 'narrow', width: 320, scheme: 'light' },
  { mode: 'rtl', width: 1280, scheme: 'light', language: 'ar-XB' },
];

/** A shot as { open, modes, fullPage }, from any of the forms a set uses. */
function shot(spec) {
  const full = typeof spec === 'object' && spec !== null ? spec : { open: spec };
  return { modes: MODES.map((m) => m.mode), fullPage: true, ...full };
}

for (const { mode, width, scheme, language } of MODES) {
  const shots = Object.entries(SET.shots).map(([type, spec]) => [type, shot(spec)]).filter(([, s]) => s.modes.includes(mode));
  if (shots.length === 0) continue;
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
    for (const [type, { open, fullPage }] of shots) {
      test(`${type} ${mode}`, async ({ page }) => {
        if (typeof open === 'function') await open(page);
        else await page.goto(open);
        // A full page capture draws a sticky action bar where the first
        // screen ends, over the form; lay it out in place for the image.
        await page.addStyleTag({ content: '.form-actions--sticky { position: static; }' });
        await page.screenshot({ path: `${SET.out}/${type}-${mode}.png`, fullPage });
      });
    }
  });
}
