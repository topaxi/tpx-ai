---
name: angular-bundle-analysis
description: Investigate the Angular initial-bundle size and the "bundle initial exceeded maximum budget" build warning. Use when asked to analyze bundle size, find what is bloating the initial/main bundle, figure out why a dependency is eagerly loaded, or estimate the savings of making something lazy. Reads the esbuild metafile (stats.json).
---

# Angular bundle analysis

Toolkit for figuring out *what ships in the initial bundle and why*, so you can
cut it down. The scripts live in `scripts/angular-bundle-analysis/` and read the
esbuild **metafile** emitted by `ng build --stats-json`. They are dependency-free
(plain `node`, no install) and operate on the static-import graph, so they
distinguish what is truly eager (ships initially) from what is lazy.

## Step 0 — generate stats

```bash
npm run build -- --source-map --stats-json --named-chunks
```

The scripts auto-detect `stats.json` under `dist/`. Pass a different path as the
last argument or via `BUNDLE_STATS=...`.

## Step 1 — see where the weight is

```bash
node scripts/angular-bundle-analysis/initial-bundle.mjs
```

Reconstructs the initial bundle (entry chunk + everything reachable through
`import-statement` edges, stopping at `dynamic-import` boundaries) and breaks it
down by npm package / app area, with the same total the budget warning reports.
Tells you whether the problem is vendor or app code, and which package is worst.

## Step 2 — drill into the worst package

```bash
node scripts/angular-bundle-analysis/package-breakdown.mjs @angular/material
```

Shows which secondary entry points / fesm chunks of that package are in the
initial bundle (e.g. `_form-field-chunk.mjs`, `select.mjs`).

## Step 3 — find out *why* a module is eager

```bash
node scripts/angular-bundle-analysis/eager-importers.mjs material/fesm2022/paginator.mjs
```

Lists every initial-graph file that statically imports the module. The culprit
is usually one of:
- a **root provider** in `src/app/app.config.ts` that imports a config token
  from a Material entry point (importing the token drags the whole module — there
  is no token-only entry point), or
- a **shared component** used by the eager app shell, or
- **another vendor module** that internally depends on it (e.g. Material's
  `paginator` pulls in `select` + `tooltip` + the CDK `overlay` module).

## Step 4 — size the fix before doing it

```bash
node scripts/angular-bundle-analysis/simulate-cut.mjs \
  material/fesm2022/paginator.mjs material/fesm2022/form-field.mjs
```

Recomputes the closure with those modules pruned (simulating moving the eager
import behind a lazy boundary) and reports the **minified** bytes that would
leave the initial bundle.

## Typical fix

For Angular: eager weight is almost always a vendor module dragged in by a
**root-level provider** or an eagerly-imported route array. Move the provider
(and its token import) to a **lazy route boundary** — a route reached via
`loadComponent`/`loadChildren`, whose `providers:` array is only instantiated
when that route activates and whose imports therefore land in a lazy chunk.
Angular route providers are inherited by child routes, so providing once at a
lazy parent (e.g. the authenticated shell) covers all descendants.

Re-run Step 0 + Step 1 after the change to confirm the new total.

## Notes / gotchas

- `bytesInOutput` is the **minified** contribution — that is the number that
  matters for the budget, not raw source bytes.
- A module only leaves the initial bundle if **all** its eager import paths are
  cut. `eager-importers.mjs` shows every path; check there isn't a second one
  (e.g. `form-field` was reachable via both a root provider *and* the paginator
  cascade).
