// Sign in once and keep the session: the login form is rate limited, and
// every test then starts signed in.
import { chromium } from '@playwright/test';
import fs from 'node:fs';

export default async function globalSetup(config) {
  const baseURL = config.projects[0].use.baseURL;
  const user = process.env.ADMIN_A11Y_USER || 'gen_admin';
  const password = process.env.ADMIN_A11Y_PASSWORD || 'b199-local-admin-pass';
  fs.mkdirSync('.auth', { recursive: true });
  const browser = await chromium.launch();
  const page = await browser.newPage();
  await page.goto(`${baseURL}/user/login`);
  await page.fill('#username', user);
  await page.fill('#password', password);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/user/login')), page.click('form [type=submit]')]);
  const response = await page.goto(`${baseURL}/admin`);
  if (response.status() !== 200) throw new Error(`signing in as ${user} did not open /admin: ${response.status()}`);
  await page.context().storageState({ path: '.auth/admin.json' });
  await browser.close();
}
