#!/usr/bin/env bash
#
# Run every fixture diff through `git-commit --dry-run --diff-file` and collect
# the generated messages into a Markdown report for human or agent judging.
#
# LLM output is non-deterministic, so this is an eval, not a pass/fail test: each
# fixture ships a `.expected.md` rubric describing what a good message should and
# should not say. Read the report and judge against the rubric.
#
# Usage:
#   evals/run.sh [--provider anthropic|ollama] [--model NAME] [extra git-commit args]
#
# Examples:
#   evals/run.sh --provider anthropic
#   evals/run.sh --provider ollama --model gemma4:12b
#
# Requires: jq, and a built git-commit binary (cargo build --release).

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
bin="${GIT_COMMIT_BIN:-$root/target/release/git-commit}"
report="$here/report.md"

if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required" >&2
  exit 1
fi
if [[ ! -x "$bin" ]]; then
  echo "error: git-commit binary not found at $bin" >&2
  echo "build it first: cargo build -p git-commit --release" >&2
  exit 1
fi

{
  echo "# git-commit eval report"
  echo
  echo "Generated: $(date -u '+%Y-%m-%d %H:%M:%SZ')"
  echo "Binary: \`$bin\`"
  echo "Args: \`$*\`"
  echo
  echo "LLM output is non-deterministic. Judge each message against its rubric."
  echo
} >"$report"

for diff in "$here"/fixtures/*.diff; do
  name="$(basename "$diff" .diff)"
  echo "running $name…" >&2

  # Capture NDJSON events; tolerate a non-zero exit so one bad fixture does not
  # abort the whole run.
  out="$("$bin" --dry-run --diff-file "$diff" "$@" 2>/dev/null || true)"

  subject="$(printf '%s\n' "$out" | jq -rs 'map(select(.kind=="subject")) | last // {} | .text // ""')"
  body="$(printf '%s\n' "$out" | jq -rs 'map(select(.kind=="body") | .text) | .[]')"
  first_progress="$(printf '%s\n' "$out" | jq -rs 'map(select(.kind=="progress")) | first // {} | .msg // ""')"
  model="$(printf '%s\n' "$out" | jq -rs 'map(select(.kind=="progress")) | first // {} | .model // "?"')"

  case "$first_progress" in
    summarizing*) path="map-reduce (large diff)" ;;
    generating\ message*) path="direct (single pass)" ;;
    *) path="unknown" ;;
  esac

  {
    echo "## $name"
    echo
    echo "- **Path:** $path"
    echo "- **Model:** $model"
    echo
    echo "### Generated message"
    echo
    echo '```'
    if [[ -z "$subject" ]]; then
      echo "(no subject produced - check provider/credentials)"
    else
      echo "$subject"
      if [[ -n "$body" ]]; then
        echo
        printf '%s\n' "$body" | sed 's/^/- /'
      fi
    fi
    echo '```'
    echo

    if [[ -f "$here/fixtures/$name.expected.md" ]]; then
      echo "### Rubric"
      echo
      cat "$here/fixtures/$name.expected.md"
      echo
    fi

    echo "### Verdict"
    echo
    echo "_pass / fail + notes:_"
    echo
  } >>"$report"
done

echo "wrote $report" >&2
cat "$report"
