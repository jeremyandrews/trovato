// One-off capture for B303: the performance page's new "Purge the CDN"
// section, the page after a tag purge showing its flash message, and one
// full-page shot, plus an axe check of the page. Not part of `npm test` or
// `npm run screenshots`; run by hand against a seeded site:
//
//   node b303-screenshots.mjs
//
// Needs ADMIN_A11Y_BASE (default http://localhost:3099), ADMIN_A11Y_USER
// and ADMIN_A11Y_PASSWORD.
import { chromium } from 'playwright';
import AxeBuilder from '@axe-core/playwright';

const BASE = process.env.ADMIN_A11Y_BASE || 'http://localhost:3099';
const USER = process.env.ADMIN_A11Y_USER || 'gen_admin';
const PASSWORD = process.env.ADMIN_A11Y_PASSWORD || 'DemoAdminPass123';
const OUT = '../../docs/screenshots/B303';

async function main() {
  const browser = await chromium.launch();
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  const page = await context.newPage();

  // Sign in.
  await page.goto(`${BASE}/user/login`);
  await page.fill('#username', USER);
  await page.fill('#password', PASSWORD);
  await Promise.all([
    page.waitForURL((u) => !u.pathname.startsWith('/user/login')),
    page.click('form [type=submit]'),
  ]);

  // The performance page with the new "Purge the CDN" section.
  await page.goto(`${BASE}/admin/config/performance`);
  await page.locator('#performance-purge').waitFor();
  await page.locator('#performance-purge').scrollIntoViewIfNeeded();
  await page.screenshot({ path: `${OUT}/01-performance-purge-section.png` });
  console.log('Saved 01-performance-purge-section.png');

  // Run an axe check on the page as it stands (before the purge submit, the
  // state most representative of ordinary admin use).
  const results = await new AxeBuilder({ page })
    .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'])
    .analyze();
  const serious = results.violations.filter((v) => v.impact === 'serious' || v.impact === 'critical');
  console.log(`axe: ${results.violations.length} total violation(s), ${serious.length} serious/critical`);
  for (const v of results.violations) {
    console.log(`  [${v.impact}] ${v.id}: ${v.help} (${v.nodes.length} node(s))`);
  }
  if (serious.length > 0) {
    throw new Error(`axe found ${serious.length} serious/critical violation(s) on /admin/config/performance`);
  }

  // Submit a tag purge ("These tags" is the form's default scope) and show
  // the flash message it leaves.
  await page.fill('#tags', 'item:00000000-0000-0000-0000-000000000000\nsite');
  await Promise.all([
    page.waitForURL('**/admin/config/performance'),
    page.click('form#cdn_purge_form [type=submit]'),
  ]);
  await page.locator('.notice').first().waitFor({ timeout: 10_000 });
  await page.screenshot({ path: `${OUT}/02-after-tag-purge.png` });
  console.log('Saved 02-after-tag-purge.png');

  // A full-page shot.
  await page.screenshot({ path: `${OUT}/03-full-page.png`, fullPage: true });
  console.log('Saved 03-full-page.png');

  await browser.close();
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
