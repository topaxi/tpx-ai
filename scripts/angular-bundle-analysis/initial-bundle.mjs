#!/usr/bin/env node
/**
 * initial-bundle.mjs — what ships in the initial bundle, and where the weight is.
 *
 * Intention
 * ---------
 * Reproduces the build's "bundle initial exceeded maximum budget" number from
 * first principles and explains it: walks the static-import graph from the entry
 * chunk, sums the minified contribution (`bytesInOutput`) of every input, and
 * aggregates it by npm package / app area. This is the first thing to run when
 * the initial budget warning appears — it tells you whether the weight is vendor
 * or app code, and which packages dominate.
 *
 * Usage
 * -----
 *   node scripts/angular-bundle-analysis/initial-bundle.mjs [path/to/stats.json]
 *   BUNDLE_STATS=dist/.../stats.json node scripts/angular-bundle-analysis/initial-bundle.mjs
 *
 * Prereq: build with stats, e.g.
 *   npm run build -- --source-map --stats-json --named-chunks
 */
import { loadStats, initialChunks, bucketOf, kb } from './lib.mjs';

const { path, stats } = loadStats(process.argv[2]);
const { outputs } = stats;
const chunks = initialChunks(outputs);

const agg = {};
let jsBytes = 0;
for (const k of chunks) {
  jsBytes += outputs[k].bytes;
  for (const [inp, info] of Object.entries(outputs[k].inputs || {})) {
    const b = bucketOf(inp);
    agg[b] = (agg[b] || 0) + info.bytesInOutput;
  }
}

const polyKey = Object.keys(outputs).find((k) => k.includes('polyfills') && k.endsWith('.js'));
const cssKey = Object.keys(outputs).find((k) => k.includes('styles-') && k.endsWith('.css'));
const poly = polyKey ? outputs[polyKey].bytes : 0;
const css = cssKey ? outputs[cssKey].bytes : 0;

console.log(`stats: ${path}`);
console.log(`initial JS chunks: ${chunks.size}\n`);

const rows = Object.entries(agg).sort((a, b) => b[1] - a[1]);
let app = 0;
let vendor = 0;
for (const [b, bytes] of rows) (b.startsWith('APP:') ? (app += bytes) : (vendor += bytes));

console.log('=== Initial bundle by package / area (minified) ===');
for (const [b, bytes] of rows.slice(0, 40)) console.log(kb(bytes) + '  ' + b);

console.log('\n=== Totals ===');
console.log(kb(app) + '  app source');
console.log(kb(vendor) + '  vendor');
console.log(kb(jsBytes) + '  initial JS');
console.log(kb(poly) + '  polyfills');
console.log(kb(css) + '  global CSS');
console.log(kb(jsBytes + poly + css) + '  INITIAL TOTAL');
