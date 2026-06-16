#!/usr/bin/env node
/**
 * simulate-cut.mjs — how much would the initial bundle shrink if <modules> were lazy?
 *
 * Intention
 * ---------
 * "What-if" estimator. Given one or more module substrings, recomputes the
 * static-import closure from the entry while pruning those modules (simulating
 * moving the provider/import that drags them in behind a lazy boundary), then
 * reports the minified bytes that would leave the initial bundle. Use it to
 * size a refactor BEFORE doing it — e.g. relocating root Material providers to
 * lazy feature routes.
 *
 * Note: this models removing the *eager edge* to the named modules. Anything
 * still reachable through another eager path stays (the report reflects that).
 *
 * Usage
 * -----
 *   node scripts/angular-bundle-analysis/simulate-cut.mjs <mod-substr> [<mod-substr> ...]
 *   BUNDLE_STATS=path node scripts/angular-bundle-analysis/simulate-cut.mjs <mod-substr> ...
 *
 * Example (the paginator + form-field provider cascade)
 *   node scripts/angular-bundle-analysis/simulate-cut.mjs \
 *     material/fesm2022/paginator.mjs material/fesm2022/form-field.mjs
 */
import { loadStats, inputClosure, initialChunks, resolveInput, findEntryInput, kb } from './lib.mjs';

const needles = process.argv.slice(2).filter((a) => !a.endsWith('.json'));
const statsArg = process.argv.slice(2).find((a) => a.endsWith('.json'));
if (needles.length === 0) {
  console.error('usage: simulate-cut.mjs <module-substring> [<module-substring> ...] [stats.json]');
  process.exit(1);
}

const { stats } = loadStats(statsArg);
const { inputs, outputs } = stats;
const cutSet = new Set(needles.map((n) => resolveInput(inputs, n)));
const entryInput = findEntryInput(outputs);

const base = inputClosure(inputs, [entryInput]);
const after = inputClosure(inputs, [entryInput], (f) => cutSet.has(f));
const removed = new Set([...base].filter((f) => !after.has(f)));

// Minified weight = bytesInOutput summed over the initial chunks only.
const chunks = initialChunks(outputs);
let min = 0;
const detail = {};
for (const k of chunks) {
  for (const [inp, info] of Object.entries(outputs[k].inputs || {})) {
    if (removed.has(inp)) {
      min += info.bytesInOutput;
      detail[inp] = (detail[inp] || 0) + info.bytesInOutput;
    }
  }
}

console.log('Cutting eager edges to:');
for (const c of cutSet) console.log('   ' + c.replace('node_modules/', 'nm:'));
console.log(`\nFiles removed from initial graph: ${removed.size}`);
console.log(`Minified savings in initial bundle: ${(min / 1024).toFixed(1)} KB\n`);

console.log('Top removed (minified, in initial chunks):');
Object.entries(detail)
  .sort((a, b) => b[1] - a[1])
  .slice(0, 20)
  .forEach(([f, b]) => console.log(kb(b) + '  ' + f.replace('node_modules/', 'nm:')));
