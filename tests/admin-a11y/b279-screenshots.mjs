// B279: screenshots of the gather builder against real, tagged content,
// never an empty state. Run by hand against a seeded site:
//
//   node b279-screenshots.mjs
//
// Needs ADMIN_A11Y_BASE (default http://localhost:3199), ADMIN_A11Y_USER and
// ADMIN_A11Y_PASSWORD, and a site seeded with `trovato generate` (two of its
// blog posts, sharing a category tag, renamed to read as recipes so the
// "related recipes" shape has something real to show, not an empty state).
import { chromium } from 'playwright';

const BASE = process.env.ADMIN_A11Y_BASE || 'http://localhost:3199';
const USER = process.env.ADMIN_A11Y_USER || 'gen_admin';
const PASSWORD = process.env.ADMIN_A11Y_PASSWORD || 'b279-local-admin-pass';
const OUT = '../../docs/screenshots/B279';

async function createQuery(page, id, label, filters) {
  const definition = {
    base_table: 'item',
    stage_aware: true,
    filters,
  };
  const display = { format: 'list', items_per_page: 20 };
  await page.goto(`${BASE}/admin/gather/create`);
  const csrfToken = await page.locator('input[name=_token]').inputValue();
  const response = await page.request.post(`${BASE}/admin/gather/create`, {
    form: {
      _token: csrfToken,
      _form_build_id: 'x',
      query_id: id,
      label,
      description: '',
      definition_json: JSON.stringify(definition),
      display_json: JSON.stringify(display),
    },
  });
  if (!response.ok()) throw new Error(`creating ${id} failed: ${await response.text()}`);
}

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

  const otherArticlesId = 'b279.other_articles_screenshot';
  const relatedRecipesId = 'b279.related_recipes_screenshot';
  await createQuery(page, otherArticlesId, 'Other articles', [
    { field: 'type', operator: 'equals', value: 'blog' },
    { field: 'id', operator: 'not_equals', value: 'current_item' },
  ]);
  await createQuery(page, relatedRecipesId, 'Related recipes', [
    { field: 'type', operator: 'equals', value: 'blog' },
    { field: 'id', operator: 'not_equals', value: 'current_item' },
    { field: 'fields.field_tags', operator: 'has_any_tag', value: { current_item_field: 'field_tags' } },
  ]);

  // 01: the "Related recipes" gather, its tag filter showing "Has any of
  // these tags" and "A field of this item: field_tags".
  await page.goto(`${BASE}/admin/gather/${relatedRecipesId}/edit`);
  await page.screenshot({ path: `${OUT}/01-related-recipes.png`, fullPage: true });
  console.log('Saved 01-related-recipes.png');

  // 02: the "Other articles" gather.
  await page.goto(`${BASE}/admin/gather/${otherArticlesId}/edit`);
  await page.screenshot({ path: `${OUT}/02-other-articles.png`, fullPage: true });
  console.log('Saved 02-other-articles.png');

  // 03: the operator menu open, on the tag filter row.
  await page.goto(`${BASE}/admin/gather/${relatedRecipesId}/edit`);
  const tagRow = page.locator('.gather-block-row--filter').nth(2);
  const operatorSelect = tagRow.locator('select').nth(0);
  await operatorSelect.focus();
  await page.keyboard.press('Alt+ArrowDown');
  await page.waitForTimeout(150);
  await page.screenshot({ path: `${OUT}/03-operator-menu-open.png` });
  console.log('Saved 03-operator-menu-open.png');
  await page.keyboard.press('Escape');

  // 04: the value source menu open, on the same row.
  const sourceSelect = tagRow.locator('select').nth(1);
  await sourceSelect.focus();
  await page.keyboard.press('Alt+ArrowDown');
  await page.waitForTimeout(150);
  await page.screenshot({ path: `${OUT}/04-value-source-menu-open.png` });
  console.log('Saved 04-value-source-menu-open.png');
  await page.keyboard.press('Escape');

  // 05: a full page shot of the builder.
  await page.screenshot({ path: `${OUT}/05-full-page.png`, fullPage: true });
  console.log('Saved 05-full-page.png');

  // Clean up the two gathers this script created.
  for (const id of [otherArticlesId, relatedRecipesId]) {
    await page.goto(`${BASE}/admin/gather/${id}/edit`);
    const token = await page.locator('input[name=_token]').inputValue();
    await page.request.post(`${BASE}/admin/gather/${id}/delete`, { form: { _token: token } });
  }

  await browser.close();
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
