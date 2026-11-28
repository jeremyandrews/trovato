// The pages to check: every GET route in docs/admin/inventory.md's kernel
// table. A route with a parameter is opened at an address found by following
// links from the page that lists it (its path up to the parameter). Only the
// routes in EXPECTED_UNSEEDED may go unopened, each for the reason given: any
// other route with no address found fails, so a listing that stops linking to
// its pages cannot quietly drop them from the checks.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const doc = fs.readFileSync(path.join(here, '../../docs/admin/inventory.md'), 'utf8');
const kernel = doc.split('<!-- kernel routes -->')[1].split('<!-- end kernel routes -->')[0];

/** Not pages: an image and JSON endpoints. */
const NOT_PAGES = new Set(['/admin/appearance/{theme}/screenshot', '/admin/stage/current', '/admin/embed/status', '/admin/search/suggest']);

export const routes = kernel
  .split('\n')
  .filter((l) => l.startsWith('| `/admin'))
  .map((l) => {
    // The table's own pipes, not the escaped ones inside a cell.
    const cells = l.split(/(?<!\\)\|/).slice(1, -1).map((c) => c.trim());
    // A route the kernel serves only while a plugin is enabled says so in
    // its gate column.
    const plugin = (cells[5] || '').match(/plugin (\w+) enabled/);
    return { path: cells[0].replaceAll('`', ''), methods: cells[1], group: cells[2], plugin: plugin ? plugin[1] : null };
  })
  .filter((r) => /GET/.test(r.methods) && !NOT_PAGES.has(r.path));

/**
 * The routes the generated site has nothing to open for, and why. Any other
 * parameterised route must be found; one listed here that is found is
 * checked like any other.
 */
export const EXPECTED_UNSEEDED = new Map([
  ['/admin/content/{id}/translate/{lang}', 'the site has one language'],
  ['/admin/content/files/{id}', 'no file is uploaded'],
  ['/admin/structure/menus/{menu}/{id}/edit', 'no menu link is generated'],
  ['/admin/structure/records/{type}', 'no record type is defined'],
  ['/admin/structure/records/{type}/{id}', 'no record type is defined'],
  ['/admin/structure/tiles/{id}/edit', 'no tile is placed'],
  ['/admin/people/rules/{id}/edit', 'no access rule is defined'],
  ['/admin/people/rules/{id}/delete', 'no access rule is defined'],
  ['/admin/system/ai-providers/{id}/edit', 'no AI provider is configured'],
  ['/admin/config/formats/{format}/delete', 'the built-in text formats cannot be deleted'],
  ['/admin/config/redirects/{id}/edit', 'no redirect is defined'],
  ['/admin/config/redirects/{id}/delete', 'no redirect is defined'],
  ['/admin/reports/timers/{id}/edit', 'no timer is set'],
  ['/admin/config/languages/{code}/delete', 'the site has one language, the default, which cannot be deleted'],
]);

/**
 * The plugins the generated site installs (scripts/seed-plugins.txt, which
 * seed-site.sh reads too). A 404 from one of their pages is a failure; only
 * another plugin's page may be absent because the plugin is off.
 */
export const SEEDED_PLUGINS = new Set(
  fs.readFileSync(path.join(here, 'scripts/seed-plugins.txt'), 'utf8').split(/\s+/).filter(Boolean),
);

/** Routes whose instances are linked from another page than their parent path. */
const LISTED_ON = {
  '/admin/structure/tags/{id}/edit': '/admin/structure/categories/{id}/tags',
  '/admin/structure/tags/{id}/delete': '/admin/structure/categories/{id}/tags',
};

/** Pages that link to no instance of a route, and where one is found. */
const SOURCES = {
  '/admin/users/{user_id}/sessions': '/admin/people/{id}/edit',
  '/admin/content/{id}/translate': '/admin/content/{id}/edit',
  '/admin/structure/types/{type}/search': '/admin/structure/types/{type}/edit',
  '/admin/structure/categories/{id}/tags/add': '/admin/structure/categories/{id}/tags',
  '/admin/system/ai-budgets/user/{id}': '/admin/people/{id}/edit',
};

const toRegex = (pattern) => new RegExp('^' + pattern.replace(/\{[^}]+\}/g, '[^/?#]+') + '$');
const resolved = new Map();

/** Why the last lookup of a pattern found nothing, for the failure message. */
const misses = new Map();

/** The links on `address`, retried: a listing that answers anything but 200
 * three times running is reported, not taken as listing nothing. */
async function linksOn(page, address) {
  let status = 0;
  for (let attempt = 0; attempt < 3; attempt++) {
    const response = await page.goto(address).catch(() => null);
    status = response ? response.status() : 0;
    if (status === 200) {
      const hrefs = await page.$$eval('a[href]', (as) => as.map((a) => a.getAttribute('href')));
      return hrefs.map((h) => h.split(/[?#]/)[0]);
    }
    await page.waitForTimeout(500);
  }
  misses.set(address, `${address} answered ${status || 'nothing'}`);
  return [];
}

/** A concrete address for `pattern`, or null. */
async function concrete(page, pattern) {
  if (!pattern.includes('{')) return pattern;
  if (resolved.has(pattern)) return resolved.get(pattern);
  let found = null;
  if (SOURCES[pattern]) {
    // The sibling's parameter values, in order, put into this pattern.
    const sibling = SOURCES[pattern];
    const source = await concrete(page, sibling);
    if (source) {
      const values = sibling.split('/').map((p, i) => (p.startsWith('{') ? source.split('/')[i] : null)).filter(Boolean);
      let i = 0;
      const candidate = pattern.replace(/{[^}]+}/g, () => values[i++]);
      const response = await page.goto(candidate);
      if (response && response.status() === 200) found = candidate;
    }
  } else if (LISTED_ON[pattern]) {
    const listing = await concrete(page, LISTED_ON[pattern]);
    if (listing) {
      const full = toRegex(pattern);
      found = (await linksOn(page, listing)).find((h) => full.test(h)) || null;
    }
  } else {
    const parts = pattern.split('/');
    let last = -1;
    parts.forEach((p, i) => { if (p.startsWith('{')) last = i; });
    const listing = await concrete(page, parts.slice(0, last).join('/') || '/admin');
    if (listing) {
      const full = toRegex(pattern);
      let links = await linksOn(page, listing);
      found = links.find((h) => full.test(h)) || null;
      // A listing that loaded but showed no such link is read once more:
      // a page the site was still settling (a cache being refilled) has
      // been seen to list none once.
      if (!found && links.length) {
        await page.waitForTimeout(1000);
        links = await linksOn(page, listing);
        found = links.find((h) => full.test(h)) || null;
      }
      if (!found) {
        // A link to the instance itself, then the rest of the path.
        const head = toRegex(parts.slice(0, last + 1).join('/'));
        const base = links.find((h) => head.test(h));
        if (base) {
          const candidate = [base, ...parts.slice(last + 1)].join('/');
          const response = await page.goto(candidate);
          if (response && response.status() === 200) found = candidate;
        }
      }
    }
  }
  // Only a found address is remembered; a miss is looked up again next time.
  if (found) resolved.set(pattern, found);
  else if (!misses.has(pattern)) misses.set(pattern, 'its listing links to none');
  return found;
}

/** Why no address was found for `pattern`. */
export function whyNoAddress(pattern) {
  const listingProblem = [...misses.entries()].find(([k]) => !k.includes('{') && pattern.startsWith(k));
  return listingProblem ? listingProblem[1] : misses.get(pattern) || 'its listing links to none';
}

/** The address to open for `route`, or null when nothing seeded matches. */
export async function addressFor(page, route) {
  return concrete(page, route.path);
}
