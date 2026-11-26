// Gate 3 of docs/design/incremental-search-index.md: the stock `pagefind.js`
// and WASM a bundle carries, run headless in Node against that bundle's index.
//
//   node query.mjs <bundle dir> <language> <queries.json>
//
// Prints one JSON object: for each query, the results in the order
// `pagefind.search` returns them, as { url, score }. `url` is the `url` meta
// (the item's path), falling back to the fragment URL. Nothing is installed:
// the bundle's own `pagefind.js` is imported, and `fetch` is answered from the
// bundle directory, so the browser code runs unmodified.
import { readFile } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

const [bundleDir, language, queriesFile] = process.argv.slice(2);
if (!bundleDir || !language || !queriesFile) {
  console.error("usage: node query.mjs <bundle dir> <language> <queries.json>");
  process.exit(2);
}
const queries = JSON.parse(await readFile(queriesFile, "utf8"));

// The globals the browser bundle touches. No `document.querySelector`, so the
// language is the one passed below, not one read from a page.
globalThis.window = {};
globalThis.document = {};
globalThis.location = { href: "http://gate.invalid/" };

const notFound = [];
globalThis.fetch = async (url) => {
  const rel = decodeURIComponent(
    new URL(url, "http://gate.invalid").pathname.replace(/^\/pagefind\//, ""),
  );
  let body;
  try {
    body = await readFile(path.join(bundleDir, rel));
  } catch {
    notFound.push(rel);
    const fail = async () => {
      throw new Error(`404 ${rel}`);
    };
    return { ok: false, status: 404, arrayBuffer: fail, json: fail };
  }
  return {
    ok: true,
    status: 200,
    arrayBuffer: async () =>
      body.buffer.slice(body.byteOffset, body.byteOffset + body.byteLength),
    json: async () => JSON.parse(body.toString("utf8")),
  };
};

const pagefind = await import(pathToFileURL(path.join(bundleDir, "pagefind.js")).href);

// A query that makes the search backend panic is recorded as an error, and
// the next query gets a fresh Pagefind (and WASM instance), so a broken one is
// never reused.
const start = async () => {
  await pagefind.destroy();
  await pagefind.options({ basePath: "/pagefind/", language });
  await pagefind.init();
};
await start();
const out = {};
for (const query of queries) {
  try {
    const search = await pagefind.search(query);
    const results = [];
    for (const result of search.results) {
      const data = await result.data();
      results.push({ url: data.meta?.url || data.url, score: result.score });
    }
    out[query] = results;
  } catch (e) {
    out[query] = { error: String(e?.message ?? e).split("\n")[0] };
    await start();
  }
}
process.stdout.write(JSON.stringify({ results: out, missing: notFound }) + "\n");
