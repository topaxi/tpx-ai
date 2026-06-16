/**
 * Shared helpers for the angular-bundle-analysis scripts.
 *
 * Reads the Angular esbuild metafile (stats.json, emitted by `ng build --stats-json`).
 * Provides helpers for walking the static-import graph to answer:
 *   - what is reachable from the entry WITHOUT crossing a lazy (dynamic-import) boundary, and
 *   - how big each contributor is once minified.
 *
 * Imported by the sibling scripts; not run directly. All scripts accept the
 * metafile path as their last positional argument or via the BUNDLE_STATS env
 * var. If neither is provided, the path is auto-detected by scanning dist/ for
 * a stats.json one level deep.
 */
import { readFileSync, existsSync, readdirSync } from 'node:fs';

function findDefaultStats() {
  if (existsSync('dist')) {
    for (const entry of readdirSync('dist', { withFileTypes: true })) {
      if (!entry.isDirectory()) continue;
      const candidate = `dist/${entry.name}/stats.json`;
      if (existsSync(candidate)) return candidate;
    }
  }
  return 'dist/stats.json';
}

export const DEFAULT_STATS = findDefaultStats();

/** Load the esbuild metafile; path from argv[2], $BUNDLE_STATS, or auto-detected. */
export function loadStats(argvPath) {
  const path = argvPath || process.env.BUNDLE_STATS || DEFAULT_STATS;
  return { path, stats: JSON.parse(readFileSync(path, 'utf8')) };
}

/** The browser entry output key (main-<hash>.js), or throws if not found. */
export function findEntry(outputs, hint = 'main-') {
  const key = Object.keys(outputs).find(
    (k) => k.includes(hint) && k.endsWith('.js') && !k.endsWith('.map'),
  );
  if (!key) throw new Error(`No entry chunk matching "${hint}" found`);
  return key;
}

/**
 * The input file that is the Angular app entry point (e.g. src/main.ts).
 * Reads the `entryPoint` field from the main output chunk in the metafile —
 * works for any Angular project without hardcoding the path.
 */
export function findEntryInput(outputs) {
  const entryKey = findEntry(outputs);
  return outputs[entryKey].entryPoint ?? 'src/main.ts';
}

/**
 * Set of OUTPUT chunk keys that form the initial bundle: the entry chunk plus
 * everything it reaches through `import-statement` edges (static imports).
 * `dynamic-import` edges (lazy routes/components) are the cut points.
 */
export function initialChunks(outputs, entryKey = findEntry(outputs)) {
  const byBase = {};
  for (const k of Object.keys(outputs)) {
    if (!k.endsWith('.map')) byBase[k.split('/').pop()] = k;
  }
  const seen = new Set();
  (function walk(base) {
    const k = byBase[base];
    if (!k || seen.has(k)) return;
    seen.add(k);
    for (const im of outputs[k].imports || []) {
      if (im.kind === 'import-statement') walk(im.path.split('/').pop());
    }
  })(entryKey.split('/').pop());
  return seen;
}

/** Static (non-lazy) input-import targets of an input file. */
export function staticImports(inputs, file) {
  return (inputs[file]?.imports || [])
    .filter((i) => i.kind !== 'dynamic-import')
    .map((i) => i.path);
}

/**
 * Set of INPUT files reachable from `roots` via static imports only.
 * `cut(file) => true` prunes a file (and its subtree) — used to simulate making
 * a module lazy / removing an eager edge.
 */
export function inputClosure(inputs, roots, cut) {
  const seen = new Set();
  const stack = [...roots];
  while (stack.length) {
    const f = stack.pop();
    if (seen.has(f) || (cut && cut(f))) continue;
    seen.add(f);
    for (const d of staticImports(inputs, f)) if (!seen.has(d)) stack.push(d);
  }
  return seen;
}

/** Resolve a fuzzy substring to a single input key, or throw with candidates. */
export function resolveInput(inputs, needle) {
  const matches = Object.keys(inputs).filter((k) => k.includes(needle));
  if (matches.length === 0) throw new Error(`No input matches "${needle}"`);
  // Prefer the shortest match (usually the canonical entry, not a sub-chunk).
  return matches.sort((a, b) => a.length - b.length)[0];
}

/** Group an npm/app path into a coarse bucket for aggregation. */
export function bucketOf(p) {
  const m = p.match(/node_modules\/(@[^/]+\/[^/]+|[^/]+)/);
  if (m) return m[1];
  if (p.startsWith('src/')) return 'APP:' + p.split('/').slice(0, 3).join('/');
  return 'OTHER:' + p;
}

export const kb = (b) => (b / 1024).toFixed(1).padStart(8) + ' KB';
