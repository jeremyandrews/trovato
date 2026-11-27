// One-off capture for B252: the new dead jobs admin page, the dashboard's
// Scheduled tasks panel linking to it, and "Who can do this?" naming a
// role's members. Not part of `npm test` or `npm run screenshots`; run by
// hand against a seeded site with at least three dead plugin-queue jobs and
// a role with more than twenty members:
//
//   node b252-screenshots.mjs
//
// Needs ADMIN_A11Y_BASE (default http://localhost:3199), ADMIN_A11Y_USER
// and ADMIN_A11Y_PASSWORD, and ADMIN_A11Y_ROLE_PATH naming the admin page
// path used to look up "Who can do this?" (default /admin/reports/dead-jobs).
import { chromium } from 'playwright';

const BASE = process.env.ADMIN_A11Y_BASE || 'http://localhost:3199';
const USER = process.env.ADMIN_A11Y_USER || 'gen_admin';
const PASSWORD = process.env.ADMIN_A11Y_PASSWORD || 'b252-screens-pass1';
const WHO_CAN_PATH = process.env.ADMIN_A11Y_ROLE_PATH || '/admin/reports/dead-jobs';
const OUT = '../../docs/images/b252';

async function signIn(context) {
  const page = await context.newPage();
  await page.goto(`${BASE}/user/login`);
  await page.fill('#username', USER);
  await page.fill('#password', PASSWORD);
  await Promise.all([
    page.waitForURL((u) => !u.pathname.startsWith('/user/login')),
    page.click('form [type=submit]'),
  ]);
  return page;
}

async function captureMode(browser, colorScheme) {
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 }, colorScheme });
  const page = await signIn(context);

  // The new dead jobs page: queue, plugin, job id, when it died, the
  // reason, attempts, and the Retry / Discard actions.
  await page.goto(`${BASE}/admin/reports/dead-jobs`);
  await page.screenshot({ path: `${OUT}/01-dead-jobs-${colorScheme}.png` });
  console.log(`Saved 01-dead-jobs-${colorScheme}.png`);

  // The Dashboard's Scheduled tasks panel, now linking to the page instead
  // of naming the JSON API.
  await page.goto(`${BASE}/admin`);
  const panel = page.locator('section[aria-labelledby="dashboard-panel-tasks"]');
  await panel.scrollIntoViewIfNeeded();
  await panel.screenshot({ path: `${OUT}/02-dashboard-scheduled-tasks-${colorScheme}.png` });
  console.log(`Saved 02-dashboard-scheduled-tasks-${colorScheme}.png`);

  // "Who can do this?" for the dead jobs page: member names, capped at
  // twenty, with "and N more" linking to the filtered People listing.
  await page.goto(`${BASE}/admin/people/who-can?path=${encodeURIComponent(WHO_CAN_PATH)}`);
  await page.screenshot({ path: `${OUT}/03-who-can-names-${colorScheme}.png` });
  console.log(`Saved 03-who-can-names-${colorScheme}.png`);

  if (colorScheme === 'light') {
    // A full-page shot of the dead jobs listing, scrolled content included.
    await page.goto(`${BASE}/admin/reports/dead-jobs`);
    await page.screenshot({ path: `${OUT}/04-dead-jobs-full-page.png`, fullPage: true });
    console.log('Saved 04-dead-jobs-full-page.png');
  }

  await context.close();
}

async function main() {
  const browser = await chromium.launch();
  await captureMode(browser, 'light');
  await captureMode(browser, 'dark');
  await browser.close();
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
