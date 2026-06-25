# git-commit evals

A small harness for judging the quality of generated commit messages against
fixed diffs - in particular, catching **hallucinated detail** (claiming changes
that are not in the diff).

LLM output is non-deterministic, so this is not a `cargo test` pass/fail suite.
Each fixture is a diff plus a `.expected.md` rubric of what a good message should
and should not say; `run.sh` generates messages and lays them next to the rubric
for a human (or another agent) to judge.

## Layout

```
fixtures/<name>.diff          a diff fed to git-commit via --diff-file
fixtures/<name>.expected.md   the judging rubric for that fixture
run.sh                        runs every fixture, writes report.md
report.md                     latest generated output (git-ignored)
```

## Running

```sh
cargo build -p git-commit --release
evals/run.sh --provider anthropic
# or a local model:
evals/run.sh --provider ollama --model gemma4:12b
```

The report prints to stdout and is written to `evals/report.md`. For each fixture
it shows the generated message, which pipeline path it took (direct single-pass
vs. map-reduce), and the rubric, with a blank verdict line to fill in.

## Adding a fixture

1. Drop a unified diff at `fixtures/<name>.diff` (copy real output from
   `git diff` or `git show <sha>`).
2. Write `fixtures/<name>.expected.md` with **Should** / **Should NOT** sections.
3. Re-run `run.sh`.

## How a fixture reaches the tool

`git-commit --diff-file <path>` reads a diff directly instead of running
`git diff --staged`, so fixtures need no scratch repo. It only generates a
message (pair it with `--dry-run`); it never commits.
