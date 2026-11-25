// axe-core, WCAG 2.0, 2.1 and 2.2 A and AA, on every GET page in
// docs/admin/inventory.md, in each mode (project): light, dark, 320 px wide,
// and the right-to-left pseudo locale.
import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { routes, addressFor, EXPECTED_UNSEEDED, SEEDED_PLUGINS } from '../routes.mjs';
import { useProjectLanguage, expectNoPageScroll } from './helpers.mjs';

const TAGS = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22a', 'wcag22aa'];

useProjectLanguage(test);

for (const route of routes) {
  test(`${route.path} (${route.group})`, async ({ page }, info) => {
    const address = await addressFor(page, route);
    const unseeded = EXPECTED_UNSEEDED.get(route.path);
    test.skip(!address && Boolean(unseeded), `${route.path}: ${unseeded}`);
    expect(address, `an address for ${route.path}: its listing links to none`).toBeTruthy();
    const response = await page.goto(address);
    // A plugin's pages do not exist while it is off. Only a plugin the seeded
    // site does not install may be off; every other page must open.
    const pluginOff = route.plugin && !SEEDED_PLUGINS.has(route.plugin);
    test.skip(pluginOff && response.status() === 404, `${route.plugin} is not installed on this site`);
    expect(response.status(), `${address} opens`).toBe(200);

    const results = await new AxeBuilder({ page }).withTags(TAGS).analyze();
    const summary = results.violations.map(
      (v) => `${v.id} (${v.impact}): ${v.help}\n` + v.nodes.slice(0, 5).map((n) => `    ${n.target.join(' ')}: ${n.failureSummary.split('\n').slice(1).join(' ')}`).join('\n'),
    );
    expect(summary, `axe violations on ${address}`).toEqual([]);

    if (info.project.name === 'narrow') await expectNoPageScroll(page);
    if (info.project.name === 'rtl') {
      await expect(page.locator('html')).toHaveAttribute('dir', 'rtl');
      await expect(page.locator('html')).toHaveAttribute('lang', 'ar-XB');
    }
  });
}

test('every route excused for having nothing seeded is a route in the inventory', () => {
  const known = new Set(routes.map((r) => r.path));
  expect([...EXPECTED_UNSEEDED.keys()].filter((p) => !known.has(p))).toEqual([]);
});
