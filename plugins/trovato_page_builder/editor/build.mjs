// Build the page builder editor into the kernel's static directory.
//
//   npm ci && npm run build
//
// One script and one stylesheet, served from /static like every other kernel
// asset (script-src 'self' needs nothing else). The output is committed, so a
// server needs no Node; CI rebuilds it and fails if the committed copy
// differs from the source.
import { build } from "esbuild";
import { copyFile, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dirname, "../../..");
const out = await mkdtemp(join(tmpdir(), "pb-editor-"));

await build({
  entryPoints: { "page-builder-editor": "src/main.tsx" },
  bundle: true,
  minify: true,
  format: "iife",
  target: ["es2020"],
  jsx: "automatic",
  define: { "process.env.NODE_ENV": '"production"' },
  legalComments: "eof",
  outdir: out,
  logLevel: "info",
});

await copyFile(join(out, "page-builder-editor.js"), join(root, "static/js/page-builder-editor.js"));
await copyFile(join(out, "page-builder-editor.css"), join(root, "static/css/page-builder-editor.css"));
await rm(out, { recursive: true });
