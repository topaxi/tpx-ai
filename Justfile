# List available recipes
default:
    @just --list

# Build all tools (release)
build:
    cargo build --release

# Build all tools (dev)
build-dev:
    cargo build

tpx-shell-dir := env_var_or_default("TPX_SHELL_DIR", env_var("HOME") / "projects" / "tpx-shell")

# Install all tools and the Claude statusLine hook
install: install-tools install-statusline-hook

# Install all tools for the current user
install-tools:
    cargo install --path tools/git-commit
    cargo install --path tools/claude-status

# Install a single tool (e.g.: just install-tool git-commit)
install-tool tool:
    cargo install --path tools/{{tool}}

# Install the Rust Claude statusLine hook from tpx-shell.
# Set as statusLine.command in ~/.claude/settings.json; point it at claude-status via TPX_STATUSLINE_CMD.
install-statusline-hook:
    cargo install --manifest-path {{tpx-shell-dir}}/daemon/Cargo.toml --bin tpx-shell-claude-statusline-hook

# Lint
check:
    cargo clippy

# Format
fmt:
    cargo fmt

# Run tests
test:
    cargo test

# Demo: API/cost billing mode (Anthropic API key, no rate limits)
demo-cost:
    #!/usr/bin/env bash
    echo '{
      "model": {"display_name": "Claude Sonnet 4.6"},
      "workspace": {"current_dir": "'"$PWD"'"},
      "context_window": {
        "used_percentage": 23.5,
        "total_input_tokens": 5200,
        "total_output_tokens": 1300,
        "current_usage": {
          "cache_read_input_tokens": 4100,
          "cache_creation_input_tokens": 800
        }
      },
      "cost": {
        "total_cost_usd": 0.042,
        "total_api_duration_ms": 95000,
        "total_lines_added": 42,
        "total_lines_removed": 7
      },
      "session_id": "demo-cost",
      "session_name": "demo: cost mode",
      "effort": {"level": "medium"}
    }' | claude-status

# Demo: rate-limited mode (Pro/Teams plan) — uses live timestamps so countdowns and pace arrows render
demo-rl:
    #!/usr/bin/env bash
    now=$(date +%s)
    printf '{
      "model": {"display_name": "Claude Opus 4.8"},
      "workspace": {"current_dir": "%s"},
      "context_window": {
        "used_percentage": 45.0,
        "total_input_tokens": 9000,
        "total_output_tokens": 2100,
        "current_usage": {
          "cache_read_input_tokens": 7200,
          "cache_creation_input_tokens": 900
        }
      },
      "rate_limits": {
        "five_hour": {"used_percentage": 45.2, "resets_at": %d},
        "seven_day": {"used_percentage": 12.0, "resets_at": %d}
      },
      "session_id": "demo-rl",
      "session_name": "demo: rate-limited",
      "effort": {"level": "high"}
    }\n' "$PWD" $((now + 7200)) $((now + 302400)) | claude-status
