#!/usr/bin/env node
/**
 * package-breakdown.mjs — drill into one package's submodules in the initial bundle.
 *
 * `initial-bundle.mjs` tells you a package is heavy (e.g. @angular/material at
 * ~240 KB); this tells you *which parts*. Aggregates the minified contribution
 * by secondary entry point / fesm chunk (button.mjs, select.mjs, ...), so you
 * can see exactly which components made it into the initial bundle.
 *
 * Usage
 *   node package-breakdown.mjs <package> [stats.json]
 *
 * Examples
 *   node package-breakdown.mjs @angular/material
 *   node package-breakdown.mjs @angular/cdk
 */
import { loadStats, initialChunks, kb } from './lib.mjs';

const pkg = process.argv[2];
if (!pkg) {
  console.error('usage: package-breakdown.mjs <package> [stats.json]');
  process.exit(1);
}

const { stats } = loadStats(process.argv[3]);
const { outputs } = stats;
const chunks = initialChunks(outputs);
const prefix = `node_modules/${pkg}/`;

const agg = {};
let total = 0;
for (const k of chunks) {
  for (const [inp, info] of Object.entries(outputs[k].inputs || {})) {
    if (!inp.startsWith(prefix)) continue;
    // Collapse `.../fesm2022/<name>.mjs` to `<name>.mjs`, else keep tail.
    const m = inp.match(/(?:fesm\d+\/)?([^/]+)$/);
    const key = m ? m[1] : inp;
    agg[key] = (agg[key] || 0) + info.bytesInOutput;
    total += info.bytesInOutput;
  }
}

console.log(`=== ${pkg} in initial bundle (minified) ===`);
for (const [name, b] of Object.entries(agg).sort((a, b) => b[1] - a[1])) {
  console.log(kb(b) + '  ' + name);
}
console.log('-----');
console.log(kb(total) + `  TOTAL (${pkg})`);
