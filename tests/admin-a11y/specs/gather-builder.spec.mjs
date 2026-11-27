// B279: the gather builder (static/js/gather-builder.js) offers every
// operator the gather engine accepts and reads every filter value shape
// (fixed, list, and each contextual spelling) back into its controls, so
// opening a gather and pressing Save without touching anything leaves its
// stored definition unchanged. That mapping is client-side JS and cannot be
// reached from a Rust test (crates/kernel/tests/gather_builder_value_roundtrip_test.rs
// covers the server side: an untouched save posting the exact JSON the
// browser would echo back). This spec is the one that actually runs the
// script.
//
// Each test creates its own gather query, through the real create form
// (never a direct API/DB write), and deletes it when done: the database is
// shared with every other check in this harness.
import { test, expect } from '@playwright/test';
import { useProjectLanguage } from './helpers.mjs';

useProjectLanguage(test);

let csrfToken;

test.beforeEach(async ({ page }) => {
  await page.goto('/admin/gather/create');
  csrfToken = await page.locator('input[name=_token]').inputValue();
});

/** Create a gather query by posting `definition`/`display` straight to the
 * create form, the way a config import or an older builder would: this
 * fixture's state is never itself run through `gather-builder.js`, so the
 * test exercises exactly the load-then-save path, not the create path. */
async function createQuery(page, id, definition, display) {
  const response = await page.request.post('/admin/gather/create', {
    form: {
      _token: csrfToken,
      _form_build_id: 'x',
      query_id: id,
      label: 'B279 gather builder spec',
      description: '',
      definition_json: JSON.stringify(definition),
      display_json: JSON.stringify(display || { format: 'table' }),
    },
  });
  expect(response.ok(), await response.text()).toBe(true);
}

async function getDefinition(page, id) {
  const response = await page.request.get(`/api/v1/query/${id}`);
  expect(response.ok(), await response.text()).toBe(true);
  const body = await response.json();
  return body.definition;
}

async function deleteQuery(page, id) {
  await page.goto(`/admin/gather/${id}/edit`);
  const token = await page.locator('input[name=_token]').inputValue();
  await page.request.post(`/admin/gather/${id}/delete`, { form: { _token: token } });
}

function uniqueId(marker) {
  return `b279.spec_${marker}_${Date.now()}_${Math.floor(Math.random() * 1e6)}`;
}

// One filter per operator the engine accepts, and per `FilterValue` shape:
// fixed scalars of every JSON type, lists, every contextual spelling, and an
// exposed filter with a label and a non-default widget (none of which the
// builder has a control for, so they must come through unchanged too).
const EVERY_SHAPE_FILTERS = [
  { field: 'title', operator: 'equals', value: 'hello' },
  { field: 'title', operator: 'not_equals', value: 42 },
  { field: 'title', operator: 'contains', value: 'foo' },
  { field: 'title', operator: 'starts_with', value: 'bar' },
  { field: 'title', operator: 'ends_with', value: 'baz' },
  { field: 'created', operator: 'greater_than', value: 1000 },
  { field: 'fields.score', operator: 'less_than', value: 3.5 },
  { field: 'fields.published_date', operator: 'greater_or_equal', value: '2024-01-01' },
  { field: 'fields.active', operator: 'less_or_equal', value: true },
  { field: 'title', operator: 'in', value: ['a', 'b'] },
  { field: 'created', operator: 'not_in', value: [1, 2, 3] },
  { field: 'fields.summary', operator: 'is_null', value: null },
  { field: 'fields.summary', operator: 'is_not_null', value: null },
  { field: 'fields.tags', operator: 'has_tag', value: '11111111-1111-1111-1111-111111111111' },
  {
    field: 'fields.tags',
    operator: 'has_any_tag',
    value: ['11111111-1111-1111-1111-111111111111', '22222222-2222-2222-2222-222222222222'],
  },
  { field: 'fields.tags', operator: 'has_all_tags', value: ['11111111-1111-1111-1111-111111111111'] },
  { field: 'fields.tags', operator: 'has_tag_or_descendants', value: '11111111-1111-1111-1111-111111111111' },
  { field: 'title', operator: 'full_text_search', value: 'search terms' },
  { field: 'title', operator: 'semantic_similarity', value: 'find similar items' },
  { field: 'fields.category', operator: { custom: 'category_tag' }, value: 'whatever a plugin wants' },
  { field: 'author_id', operator: 'equals', value: 'current_user' },
  { field: 'created', operator: 'greater_than', value: 'current_time' },
  { field: 'fields.published_date', operator: 'greater_or_equal', value: 'current_date' },
  { field: 'fields.category', operator: 'equals', value: { url_arg: 'cat' } },
  { field: 'id', operator: 'not_equals', value: 'current_item' },
  { field: 'fields.tags', operator: 'has_any_tag', value: { current_item_field: 'tags' } },
  {
    field: 'fields.region',
    operator: 'equals',
    value: '1',
    exposed: true,
    exposed_label: 'Region',
    widget: { type: 'boolean' },
  },
];

test('opening a gather and pressing Save without touching anything leaves every operator and value shape unchanged', async ({
  page,
}) => {
  const id = uniqueId('everyshape');
  const definition = { base_table: 'item', stage_aware: true, filters: EVERY_SHAPE_FILTERS };
  await createQuery(page, id, definition);
  const before = await getDefinition(page, id);

  await page.goto(`/admin/gather/${id}/edit`);
  // Every filter row rendered, none left the default (B279's original bug:
  // an operator missing from the menu silently became "Equals").
  const rows = page.locator('.gather-block-row--filter');
  await expect(rows).toHaveCount(EVERY_SHAPE_FILTERS.length);

  await Promise.all([page.waitForURL('**/admin/gather'), page.locator('#gather-form [type=submit]').click()]);

  const after = await getDefinition(page, id);
  await deleteQuery(page, id);

  expect(after).toEqual(before);
});

test('the value source select shows a field of this item for current_item_field, and This item (id) for current_item', async ({
  page,
}) => {
  const id = uniqueId('related');
  const definition = {
    base_table: 'item',
    stage_aware: true,
    filters: [
      { field: 'id', operator: 'not_equals', value: 'current_item' },
      { field: 'fields.tags', operator: 'has_any_tag', value: { current_item_field: 'field_tags' } },
    ],
  };
  await createQuery(page, id, definition);

  await page.goto(`/admin/gather/${id}/edit`);
  const rows = page.locator('.gather-block-row--filter');
  await expect(rows).toHaveCount(2);

  const firstSource = rows.nth(0).locator('select').nth(1);
  await expect(firstSource).toHaveValue('current_item');

  const secondSource = rows.nth(1).locator('select').nth(1);
  await expect(secondSource).toHaveValue('current_item_field');
  const fieldInput = rows.nth(1).locator('[data-role="value-item-field"]');
  await expect(fieldInput).toHaveValue('field_tags');
  await expect(fieldInput).toBeVisible();

  // Nothing touched: saving still round-trips both filters unchanged.
  const before = await getDefinition(page, id);
  await Promise.all([page.waitForURL('**/admin/gather'), page.locator('#gather-form [type=submit]').click()]);
  const after = await getDefinition(page, id);
  await deleteQuery(page, id);
  expect(after).toEqual(before);
});

test('a fixed value spelled exactly like a contextual keyword is refused, not silently stored as contextual', async ({
  page,
}) => {
  const id = uniqueId('reserved');
  await createQuery(page, id, {
    base_table: 'item',
    stage_aware: true,
    filters: [{ field: 'title', operator: 'equals', value: 'placeholder' }],
  });

  await page.goto(`/admin/gather/${id}/edit`);
  const row = page.locator('.gather-block-row--filter').first();
  await row.locator('[data-role="value-fixed"]').fill('current_user');

  await page.locator('#gather-form [type=submit]').click();
  // Blocked client-side: still on the edit page, with a visible error.
  await expect(page).toHaveURL(new RegExp(`/admin/gather/${id}/edit$`));
  await expect(row.locator('.gather-filter-row__error')).toBeVisible();

  const stored = await getDefinition(page, id);
  expect(stored.filters[0].value).toBe('placeholder');

  await deleteQuery(page, id);
});

test('an operator the builder cannot show (a plugin\'s custom operator) is read only and kept verbatim', async ({
  page,
}) => {
  const id = uniqueId('customop');
  const definition = {
    base_table: 'item',
    stage_aware: true,
    filters: [{ field: 'fields.category', operator: { custom: 'category_tag' }, value: 'news' }],
  };
  await createQuery(page, id, definition);
  const before = await getDefinition(page, id);

  await page.goto(`/admin/gather/${id}/edit`);
  const row = page.locator('.gather-block-row--filter').first();
  await expect(row).toHaveClass(/gather-filter-row--unknown/);
  await expect(row.locator('select').first()).toBeDisabled();
  await expect(row.locator('.gather-filter-row__hint')).toBeVisible();

  await Promise.all([page.waitForURL('**/admin/gather'), page.locator('#gather-form [type=submit]').click()]);
  const after = await getDefinition(page, id);
  await deleteQuery(page, id);
  expect(after).toEqual(before);
});
