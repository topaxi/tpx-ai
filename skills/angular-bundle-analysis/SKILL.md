---
name: angular-bundle-analysis
description: Investigate the Angular initial-bundle size and the "bundle initial exceeded maximum budget" build warning. Use when asked to analyze bundle size, find what is bloating the initial/main bundle, figure out why a dependency is eagerly loaded, or estimate the savings of making something lazy. Reads the esbuild metafile (stats.json).
---

# Angular bundle analysis

Toolkit for figuring out *what ships in the initial bundle and why*, so you can cut it down. Scripts are bundled in this skill's `scripts/` directory — dependency-free (plain `node`, no install), operating on the static-import graph to distinguish what is truly eager from what is lazy.

## Locating the scripts

Before running any command, find the skill's script directory and store it as `SCRIPTS`:

```bash
for d in ".claude/skills/angular-bundle-analysis/scripts" "$HOME/.claude/skills/angular-bundle-analysis/scripts"; do
  [ -d "$d" ] && echo "$d" && break
done
```

Run all commands from the Angular **project root** using the path printed above as `SCRIPTS`.

## Step 0 — generate stats

```bash
npm run build -- --source-map --stats-json --named-chunks
```

Scripts auto-detect `stats.json` under `dist/`. Override with a path as the last argument or via `BUNDLE_STATS=<path>`.

## Step 1 — see where the weight is

```bash
node $SCRIPTS/initial-bundle.mjs
```

Reconstructs the initial bundle and breaks it down by npm package / app area using minified contribution (`bytesInOutput`). Tells you whether the problem is vendor or app code, and which package dominates.

## Step 2 — drill into the worst package

```bash
node $SCRIPTS/package-breakdown.mjs @angular/material
```

Shows which secondary entry points / fesm chunks of that package are in the initial bundle (e.g. `button.mjs`, `select.mjs`).

## Step 3 — find out *why* a module is eager

```bash
node $SCRIPTS/eager-importers.mjs material/fesm2022/paginator.mjs
```

Lists every initial-graph file that statically imports the module. Common culprits:
- A **root provider** in `app.config.ts` that imports a config token from a Material entry point (importing the token drags the whole module)
- A **shared component** used by the eager app shell
- **Another vendor module** that internally depends on it (e.g. Material's `paginator` pulls in `select` + `tooltip` + the CDK `overlay` module)

## Step 4 — size the fix before doing it

```bash
node $SCRIPTS/simulate-cut.mjs \
  material/fesm2022/paginator.mjs material/fesm2022/form-field.mjs
```

Recomputes the closure with those modules pruned (simulating moving the eager import behind a lazy boundary) and reports the **minified** bytes that would leave the initial bundle.

## Typical fix

Eager weight is almost always a vendor module dragged in by a **root-level provider** or an eagerly-imported route array. Move the provider (and its token import) to a **lazy route boundary** — a route reached via `loadComponent`/`loadChildren`, whose `providers:` array is only instantiated when that route activates. Angular route providers are inherited by child routes, so providing once at a lazy parent (e.g. the authenticated shell) covers all descendants.

Re-run Step 0 + Step 1 after the change to confirm the new total.

## Notes / gotchas

- `bytesInOutput` is the **minified** contribution — the number that matters for the budget, not raw source bytes.
- A module only leaves the initial bundle if **all** its eager import paths are cut. `eager-importers.mjs` shows every path; check there isn't a second one (e.g. `form-field` was reachable via both a root provider *and* the paginator cascade).
