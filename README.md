# tpx-ai

A collection of personal AI-powered tools and Claude skill definitions, including [`git-commit`](tools/git-commit/README.md) for AI-generated commit messages.

## Neovim Plugin

`lua/git-commit-ai/` is a Neovim plugin that hooks into `gitcommit` buffers and auto-fills commit messages using the `git-commit` binary's `--dry-run` mode.

### Prerequisites

Build and install the binary first:

```sh
cargo build --release -p git-commit
# binary lands at: target/release/git-commit
# optionally install to PATH:
cargo install --path tools/git-commit
```

### Installation (lazy.nvim)

```lua
{
  dir = "~/projects/tpx-ai",
  name = "git-commit-ai",
  ft = "gitcommit",
  config = function()
    require("git-commit-ai").setup({
      bin      = vim.fn.expand("~/projects/tpx-ai/target/release/git-commit"),
      provider = "ollama",   -- "anthropic" or "ollama" (default: binary default)
      -- model   = "gemma4:12b",
      -- context = "closes #42",
      -- keymap  = "<leader>gc",        -- normal-mode re-trigger (default: "<leader>gc")
      -- virtual_text = "Comment",      -- highlight group, or false to disable
    })
  end,
}
```

### Configuration options

| Option | Type | Default | Description |
|---|---|---|---|
| `bin` | `string` | `"git-commit"` | Path or name of the binary (must be on `PATH` if bare name) |
| `provider` | `string\|nil` | `nil` | `"anthropic"` or `"ollama"`; `nil` uses the binary's default |
| `model` | `string\|nil` | `nil` | Model override; `nil` uses the binary's default |
| `context` | `string\|nil` | `nil` | Extra context passed via `--context` |
| `keymap` | `string\|nil` | `"<leader>gc"` | Normal-mode key to re-trigger generation in `gitcommit` buffers |
| `virtual_text` | `boolean\|string` | `"Comment"` | Highlight group for the in-progress hint, or `false` to disable |

### Behaviour

- Triggers automatically when a `gitcommit` buffer opens with no existing content.
- Any keystroke while generating cancels the job.
- `keymap` re-triggers generation at any time in normal mode.
- Does nothing if the buffer already has content (`--fixup`, amend, `-m`, etc.).
- Progress is reported via `LspProgress` events (visible in Noice and similar plugins).
