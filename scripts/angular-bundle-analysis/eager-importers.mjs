#!/usr/bin/env node
/**
 * eager-importers.mjs — why is <module> in the initial bundle?
 *
 * Intention
 * ---------
 * Given a module substring (e.g. `material/fesm2022/form-field.mjs` or `luxon`),
 * lists every input file in the initial (statically-reachable) graph that
 * imports it. This answers "what is dragging this dependency into the initial
 * bundle?" — usually a root provider, a shared component, or another vendor
 * module that internally depends on it (e.g. Material's paginator pulls in
 * select + tooltip). The fix is then to move that eager importer behind a lazy
 * boundary.
 *
 * Usage
 * -----
 *   node scripts/angular-bundle-analysis/eager-importers.mjs <module-substring> [stats.json]
 *
 * Example
 *   node scripts/angular-bundle-analysis/eager-importers.mjs material/fesm2022/paginator.mjs
 */
import { loadStats, inputClosure, staticImports, resolveInput, findEntryInput } from './lib.mjs';

const needle = process.argv[2];
if (!needle) {
  console.error('usage: eager-importers.mjs <module-substring> [stats.json]');
  process.exit(1);
}

const { stats } = loadStats(process.argv[3]);
const { inputs, outputs } = stats;
const target = resolveInput(inputs, needle);
const entryInput = findEntryInput(outputs);
const eager = inputClosure(inputs, [entryInput]);

console.log(`target: ${target}`);
console.log(`eager (statically reachable from ${entryInput}): ${eager.has(target)}\n`);

const importers = [...eager]
  .filter((f) => staticImports(inputs, f).includes(target))
  .sort();

if (importers.length === 0) {
  console.log('No eager importers — only reached via dynamic-import (lazy). Good.');
} else {
  console.log(`Eager importers (${importers.length}):`);
  for (const f of importers) console.log('   ' + f.replace('node_modules/', 'nm:'));
}
