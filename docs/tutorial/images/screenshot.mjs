#!/usr/bin/env node
// Screenshot capture script for Trovato tutorial
// Usage: node screenshot.mjs <action> [args...]
//
// Actions:
//   page <url> <output> [width] [height]  — capture a page
//   login                                  — log in as admin
//   scroll <url> <output> <selector>       — scroll to element then capture
//   clip <url> <output> <selector>         — capture only a CSS selector

import { chromium } from 'playwright';

const BASE = 'http://localhost:3000';
const COOKIE_FILE = '/tmp/trovato-pw-state.json';
const WIDTH = 1200;
const HEIGHT = 800;

async function run() {
  const [,, action, ...args] = process.argv;

  const browser = await chromium.launch();
  let context;

  // Try to reuse saved auth state
  try {
    if (action !== 'login' && action !== 'install') {
      context = await browser.newContext({
        storageState: COOKIE_FILE,
        viewport: { width: WIDTH, height: HEIGHT },
      });
    }
  } catch {
    // No saved state yet
  }

  if (!context) {
    context = await browser.newContext({
      viewport: { width: WIDTH, height: HEIGHT },
    });
  }

  const page = await context.newPage();

  try {
    switch (action) {
      case 'install': {
        // Capture installer page (no login needed)
        const [url, output] = args;
        await page.goto(BASE + (url || '/'), { waitUntil: 'networkidle' });
        await page.screenshot({ path: output, fullPage: false });
        console.log(`Saved: ${output}`);
        break;
      }

      case 'login': {
        // Log in as admin and save state
        const [user, pass] = args;
        await page.goto(BASE + '/user/login', { waitUntil: 'networkidle' });
        await page.fill('input[name="username"]', user || 'admin');
        await page.fill('input[name="password"]', pass || 'trovato-admin1');
        await page.click('button[type="submit"], input[type="submit"]');
        await page.waitForURL(/.*/, { timeout: 5000 });
        await context.storageState({ path: COOKIE_FILE });
        console.log('Logged in and saved state');
        break;
      }

      case 'page': {
        const [url, output, w, h] = args;
        if (w && h) {
          await page.setViewportSize({ width: parseInt(w), height: parseInt(h) });
        }
        await page.goto(BASE + url, { waitUntil: 'networkidle', timeout: 15000 });
        await page.waitForTimeout(500); // Let animations settle
        await page.screenshot({ path: output, fullPage: false });
        console.log(`Saved: ${output}`);
        break;
      }

      case 'fullpage': {
        const [url, output, w] = args;
        if (w) {
          await page.setViewportSize({ width: parseInt(w), height: HEIGHT });
        }
        await page.goto(BASE + url, { waitUntil: 'networkidle', timeout: 15000 });
        await page.waitForTimeout(500);
        await page.screenshot({ path: output, fullPage: true });
        console.log(`Saved: ${output}`);
        break;
      }

      case 'clip': {
        const [url, output, selector] = args;
        await page.goto(BASE + url, { waitUntil: 'networkidle', timeout: 15000 });
        await page.waitForTimeout(500);
        const el = await page.$(selector);
        if (el) {
          await el.screenshot({ path: output });
        } else {
          // Fallback to full page
          await page.screenshot({ path: output, fullPage: false });
        }
        console.log(`Saved: ${output}`);
        break;
      }

      default:
        console.error(`Unknown action: ${action}`);
        process.exit(1);
    }
  } finally {
    await browser.close();
  }
}

run().catch(e => { console.error(e); process.exit(1); });
