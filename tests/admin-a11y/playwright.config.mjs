// Browser checks of the administration: axe (WCAG 2.2 A and AA) on every
// inventoried page in four modes, keyboard behaviour, and the accessibility
// tree of each page type. See docs/admin/accessibility.md.
//
// Needs a running site: ADMIN_A11Y_BASE (default http://localhost:3199),
// started with TROVATO_PSEUDO_LOCALES=1, seeded with `trovato generate`, and
// an administrator ADMIN_A11Y_USER / ADMIN_A11Y_PASSWORD.
import { defineConfig, devices } from '@playwright/test';

const baseURL = process.env.ADMIN_A11Y_BASE || 'http://localhost:3199';
const desktop = { ...devices['Desktop Chrome'], viewport: { width: 1280, height: 900 } };

export default defineConfig({
  testDir: './specs',
  // One worker: the right-to-left mode changes the admin's language
  // preference, which every mode shares. With one worker a spec file runs
  // whole, so its beforeAll (set the language) and afterAll (put it back)
  // bracket only its own tests.
  workers: 1,
  fullyParallel: false,
  timeout: 60_000,
  reporter: [['list'], ['json', { outputFile: 'test-results/results.json' }]],
  globalSetup: './global-setup.mjs',
  use: {
    baseURL,
    storageState: '.auth/admin.json',
    trace: 'retain-on-failure',
  },
  projects: [
    { name: 'light', use: { ...desktop, colorScheme: 'light' } },
    { name: 'dark', use: { ...desktop, colorScheme: 'dark' } },
    {
      name: 'narrow',
      use: { ...desktop, viewport: { width: 320, height: 720 }, colorScheme: 'light' },
    },
    {
      name: 'rtl',
      use: { ...desktop, colorScheme: 'light' },
      metadata: { language: 'ar-XB' },
    },
    // Not part of `npm test`: `npm run screenshots` writes the review images.
    { name: 'screenshots', testDir: './screenshots', use: { ...desktop } },
  ],
});
