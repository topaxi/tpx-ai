# tpx-ai

A collection of personal AI-powered tools, Claude skills, and agent definitions.

## Architecture

```
crates/llm/          Shared LLM client — Anthropic and Ollama providers
tools/<name>/        Binary crates, one per tool
skills/<name>/       Claude skill definitions (SKILL.md + resources)
agents/              Claude agent YAML definitions (for `ant` CLI)
```

## Building & Running

```sh
# Build everything
cargo build --release

# Run a tool
cargo run -p git-commit -- --help

# Build a specific tool
cargo build -p git-commit --release
```

## LLM Providers

Tools support two providers, selectable via `--provider`:

| Provider | Flag | Auth | Default model |
|---|---|---|---|
| Anthropic (cloud) | `--provider anthropic` | `ANTHROPIC_API_KEY` env var | `claude-haiku-4-5` |
| Ollama (local) | `--provider ollama` | none (local) | `qwen2.5-coder` |

```sh
export ANTHROPIC_API_KEY=sk-ant-...
export OLLAMA_HOST=http://localhost:11434   # optional, this is the default
```

## Adding a New Tool

1. Create a new binary crate under `tools/`:
   ```sh
   cargo new --bin tools/my-tool
   ```
2. Add it to the workspace in `Cargo.toml`:
   ```toml
   [workspace]
   members = ["crates/llm", "tools/git-commit", "tools/my-tool"]
   ```
3. Add `llm = { path = "../../crates/llm" }` to its `[dependencies]`.
4. Use `LlmProvider` from the `llm` crate for all LLM calls.

## Skills

Skills live in `skills/<name>/SKILL.md`. They follow the Claude skill format:

```
---
name: <slug>
description: <one-line description shown in context>
---

<skill content>
```

Register skills on a Claude agent with the `ant` CLI:
```sh
ant beta:agents update --agent-id $AGENT_ID < agents/my-agent.agent.yaml
```

## Agents

Agent YAML files in `agents/` are applied with the Anthropic CLI:

```sh
# Create once
AGENT_ID=$(ant beta:agents create < agents/my-agent.agent.yaml --transform id -r)

# Update
ant beta:agents update --agent-id "$AGENT_ID" --version N < agents/my-agent.agent.yaml
```

## Common Commands

```sh
cargo fmt                    # format all crates
cargo clippy                 # lint all crates
cargo test                   # run all tests
cargo build --release        # production build
```
