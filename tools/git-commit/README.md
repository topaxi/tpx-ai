# git-commit

AI-generated git commit messages. Stages a diff, summarises each changed file,
then generates a subject line. Commits immediately, or prints the message with
`--dry-run` (used by the Neovim plugin).

## Install

```sh
cargo build --release -p git-commit
# binary at: target/release/git-commit
```

## Usage

```sh
git-commit                          # generate and commit
git-commit --dry-run                # print message, don't commit
git-commit --provider ollama        # use local Ollama instead of Anthropic
git-commit -c "closes #42"         # pass extra context to the model
git-commit --include 'src/*.rs'     # restrict to matching files
```

## Providers

| Provider | Flag | Auth |
|---|---|---|
| Anthropic (default) | `--provider anthropic` | `ANTHROPIC_API_KEY` env var |
| Ollama (local) | `--provider ollama` | none |

Model selection: `--model <name>`, `ANTHROPIC_MODEL` / `OLLAMA_MODEL` env vars,
or `anthropic.model` / `ollama.model` in the config file.

For Ollama, if no model is configured the tool queries the running instance and
picks the best available from a ranked preference list.

## Configuration

Global config: `$XDG_CONFIG_HOME/tpx-ai/config.toml` (typically
`~/.config/tpx-ai/config.toml`).

Per-project overrides live in `[[projects]]` entries keyed by the git root
path. Paths support `~` and `$VAR` / `${VAR}` expansion.

```toml
[anthropic]
model = "claude-haiku-4-5"

[ollama]
model = "gemma4:e4b"

[commit]
format = "conventional"   # "conventional" (default) or "scoped"

[[projects]]
path = "~/work/myrepo"
commit.format = "scoped"

[[projects]]
path = "~/work/jira-project"
commit.format = "scoped"
commit.prompt_extra = """
Branch names follow 'feature/PROJ-1234-short-description'. \
Use the ticket ID (e.g. PROJ-1234) as the scope.\
"""
```

### Commit formats

**`conventional`** (default) — `type(scope): description`
```
feat(auth): add OAuth2 login flow
fix(api): handle empty response body
```

**`scoped`** — `scope: description`, scope derived from changed paths
```
git-commit: add --dry-run flag
net/http: fix redirect loop
```

### `commit.prompt_extra`

Appended to the system prompt verbatim. Useful when a project has naming
conventions the model can't infer from the diff alone — for example, deriving
a scope from the branch name. The current branch is always included in the
model's context as `Branch: <name>`.

## Neovim plugin

See [`lua/git-commit-ai/`](../../lua/git-commit-ai/) — hooks into `gitcommit`
buffers to auto-fill the message via `--dry-run`.
