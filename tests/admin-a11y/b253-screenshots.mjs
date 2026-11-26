// One-off capture for B253: the AI provider page after a connection test,
// and the gather builder with a filter row, both under the accented pseudo
// locale (en-XA), plus one full-page shot. Not part of `npm test` or `npm
// run screenshots`; run by hand against a seeded site:
//
//   node b253-screenshots.mjs
//
// Needs ADMIN_A11Y_BASE (default http://localhost:3199), ADMIN_A11Y_USER
// and ADMIN_A11Y_PASSWORD, and the site started with TROVATO_PSEUDO_LOCALES=1.
import { chromium } from 'playwright';

const BASE = process.env.ADMIN_A11Y_BASE || 'http://localhost:3199';
const USER = process.env.ADMIN_A11Y_USER || 'gen_admin';
const PASSWORD = process.env.ADMIN_A11Y_PASSWORD || 'b253-screens-pass1';
const OUT = '../../docs/images/b253';

async function main() {
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });

  // Sign in.
  await page.goto(`${BASE}/user/login`);
  await page.fill('#username', USER);
  await page.fill('#password', PASSWORD);
  await Promise.all([
    page.waitForURL((u) => !u.pathname.startsWith('/user/login')),
    page.click('form [type=submit]'),
  ]);

  // Accented pseudo locale: every translated word in brackets and accented,
  // so a literal that bypassed t() stands out as the one unmarked, unaccented
  // phrase on the page.
  await page.goto(`${BASE}/admin/preferences`);
  await page.selectOption('#admin-language', 'en-XA');
  await Promise.all([
    page.waitForURL('**/admin/preferences'),
    page.click('form#admin-preferences [type=submit]'),
  ]);

  // Add a provider whose base URL always fails to connect (a reserved,
  // never-resolving TLD), so "Test" has something to show.
  await page.goto(`${BASE}/admin/system/ai-providers/add`);
  await page.fill('#label', 'Screenshot Provider');
  await page.selectOption('#protocol', 'open_ai_compatible');
  await page.fill('#base_url', 'https://api.example.invalid/v1');
  await page.fill('#api_key_env', 'SCREENSHOT_API_KEY');
  await page.selectOption('#op_0', 'chat');
  await page.fill('#model_0', 'gpt-4o');
  await page.check('#enabled');
  await Promise.all([
    page.waitForURL('**/admin/system/ai-providers'),
    page.click('form[action="/admin/system/ai-providers/add"] [type=submit]'),
  ]);

  // Run the connection test and wait for its (translated) result.
  await page.locator('.js-test-connection button').first().click();
  await page.locator('#test-message:not(:empty)').waitFor({ timeout: 15_000 });
  // The fetch races the DNS failure; give the message a moment to settle
  // from "Testing connection..." to the final failure text.
  await page.waitForTimeout(1500);
  await page.screenshot({ path: `${OUT}/01-ai-provider-after-test.png` });
  console.log('Saved 01-ai-provider-after-test.png');

  // The gather builder: add a filter row so the operator option list, the
  // placeholders and the remove button (all script-built) are on screen.
  await page.goto(`${BASE}/admin/gather/create`);
  await page.click('#add-filter');
  await page.click('#add-sort');
  await page.click('#add-relationship');
  await page.screenshot({ path: `${OUT}/02-gather-builder.png`, fullPage: true });
  console.log('Saved 02-gather-builder.png');

  // A full-page shot (the AI provider page, scrolled content included).
  await page.goto(`${BASE}/admin/system/ai-providers`);
  await page.screenshot({ path: `${OUT}/03-full-page.png`, fullPage: true });
  console.log('Saved 03-full-page.png');

  // Put the language back and remove the test provider.
  await page.goto(`${BASE}/admin/preferences`);
  await page.selectOption('#admin-language', 'en');
  await Promise.all([
    page.waitForURL('**/admin/preferences'),
    page.click('form#admin-preferences [type=submit]'),
  ]);

  await browser.close();
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
