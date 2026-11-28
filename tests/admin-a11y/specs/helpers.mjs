import { expect } from '@playwright/test';

/** Set the signed-in admin's language preference (and nothing else). */
export async function setLanguage(page, language) {
  await page.goto('/admin/preferences');
  await page.selectOption('#language', language);
  await Promise.all([page.waitForURL('**/admin/preferences'), page.click('form#admin_preferences_form [type=submit]')]);
}

/** Before each test in the rtl project: the right-to-left pseudo locale. */
export function useProjectLanguage(test) {
  test.beforeAll(async ({ browser }, info) => {
    const language = info.project.metadata?.language;
    if (!language) return;
    const page = await browser.newPage({ storageState: '.auth/admin.json', baseURL: info.project.use.baseURL });
    await setLanguage(page, language);
    await page.close();
  });
  test.afterAll(async ({ browser }, info) => {
    if (!info.project.metadata?.language) return;
    const page = await browser.newPage({ storageState: '.auth/admin.json', baseURL: info.project.use.baseURL });
    await setLanguage(page, 'en');
    await page.close();
  });
}

/** No horizontal page scroll (WCAG 1.4.10 reflow, at 320 CSS pixels). */
export async function expectNoPageScroll(page) {
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, 'the page scrolls sideways').toBeLessThanOrEqual(0);
}
