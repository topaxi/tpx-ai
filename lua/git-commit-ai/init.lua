local M = {}

-- ── Config ────────────────────────────────────────────────────────────────────

---@class GitCommitAIConfig
---@field bin string        binary name or path; must be on PATH if a bare name
---@field provider string?  "anthropic"|"ollama"|nil (binary default)
---@field model string?     model override, or nil for binary default
---@field context string?   passed via --context
---@field virtual_text boolean|string  false=off, true/"HLGroup"=highlight group
---@field keymap string?   normal-mode re-trigger key in gitcommit buffers

local defaults = {
  bin = "git-commit",
  provider = nil,
  model = nil,
  context = nil,
  virtual_text = "Comment",
  keymap = "<leader>gc",
}

local cfg = {} ---@type GitCommitAIConfig

-- ── Namespace ─────────────────────────────────────────────────────────────────

local ns = vim.api.nvim_create_namespace("git_commit_ai")

-- ── Fake LSP client shim ──────────────────────────────────────────────────────
-- Noice's $/progress handler calls vim.lsp.get_client_by_id and silently drops
-- events for unknown clients. We monkey-patch it with a sentinel, then restore.

local FAKE_ID = -999
local _orig_get_client = nil
local _client_refs = 0

local function acquire_fake_client()
  if _client_refs == 0 then
    _orig_get_client = vim.lsp.get_client_by_id
    vim.lsp.get_client_by_id = function(id)
      if id == FAKE_ID then
        return { id = FAKE_ID, name = "git-commit-ai" }
      end
      return _orig_get_client(id)
    end
  end
  _client_refs = _client_refs + 1
end

local function release_fake_client()
  _client_refs = _client_refs - 1
  if _client_refs == 0 and _orig_get_client then
    -- Noice defers its "end" close by 100 ms; let that fire before we restore.
    vim.defer_fn(function()
      if _client_refs == 0 then
        vim.lsp.get_client_by_id = _orig_get_client
        _orig_get_client = nil
      end
    end, 200)
  end
end

local function emit_progress(kind, token, title, message)
  pcall(vim.api.nvim_exec_autocmds, "LspProgress", {
    pattern = tostring(FAKE_ID) .. "/" .. kind .. "/" .. tostring(token),
    data = {
      client_id = FAKE_ID,
      result = {
        token = token,
        value = { kind = kind, title = title, message = message },
      },
    },
  })
end

-- ── Per-buffer state ──────────────────────────────────────────────────────────

local jobs   = {} ---@type table<integer, integer>
local marks  = {} ---@type table<integer, integer>
local tokens = {} ---@type table<integer, string>
local gens   = {} ---@type table<integer, integer>

-- ── Helpers ───────────────────────────────────────────────────────────────────

local function virt_hl()
  local v = cfg.virtual_text
  if not v then return nil end
  return type(v) == "string" and v or "Comment"
end

local function has_user_content(lines)
  for _, l in ipairs(lines) do
    if l:match("^#") then break end
    if l ~= "" then return true end
  end
  return false
end

local function insert_message(bufnr, subject, body_lines)
  if not vim.api.nvim_buf_is_valid(bufnr) then return end
  local lines = vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)
  if has_user_content(lines) then return end

  local first_comment = #lines + 1
  for i, l in ipairs(lines) do
    if l:match("^#") then first_comment = i; break end
  end

  local new = { subject, "" }
  vim.list_extend(new, body_lines)
  new[#new + 1] = ""
  for i = first_comment, #lines do new[#new + 1] = lines[i] end

  vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, new)
  local winid = vim.fn.bufwinid(bufnr)
  if winid ~= -1 then
    pcall(vim.api.nvim_win_set_cursor, winid, { 1, #subject })
  end
end

local function clear(bufnr)
  if jobs[bufnr] then
    vim.fn.jobstop(jobs[bufnr])
    jobs[bufnr] = nil
  end
  if marks[bufnr] then
    pcall(vim.api.nvim_buf_del_extmark, bufnr, ns, marks[bufnr])
    marks[bufnr] = nil
  end
  if tokens[bufnr] then
    emit_progress("end", tokens[bufnr], "git-commit-ai", nil)
    tokens[bufnr] = nil
    release_fake_client()
  end
  gens[bufnr] = (gens[bufnr] or 0) + 1
  pcall(vim.api.nvim_del_augroup_by_name, "GitCommitAIAbort" .. bufnr)
end

-- ── Line accumulator ──────────────────────────────────────────────────────────
-- jobstart with buffered=false delivers chunks that may span line boundaries.
-- Accumulate in a single-element table (acts as a mutable string ref) and
-- extract complete lines on each call.

local function make_feeder(handler)
  local raw = { "" }
  local function feed(data)
    raw[1] = raw[1] .. table.concat(data, "\n")
    local ls = vim.split(raw[1], "\n")
    raw[1] = table.remove(ls) -- keep last (possibly partial) line
    for _, l in ipairs(ls) do handler(l) end
  end
  return feed, raw -- raw exposed so spinner can read the partial
end

-- ── Generator ─────────────────────────────────────────────────────────────────

function M.generate(bufnr)
  bufnr = bufnr or vim.api.nvim_get_current_buf()

  clear(bufnr) -- stops previous job and closes its Noice progress

  local token = "gca-" .. bufnr
  local my_gen = (gens[bufnr] or 0) + 1
  gens[bufnr] = my_gen

  acquire_fake_client()
  tokens[bufnr] = token
  emit_progress("begin", token, "git-commit-ai", "starting…")

  local hl = virt_hl()
  if hl then
    marks[bufnr] = vim.api.nvim_buf_set_extmark(bufnr, ns, 0, 0, {
      virt_text = { { "  generating commit message... type your own to cancel.", hl } },
      virt_text_pos = "eol",
    })
  end

  -- Per-job closure state
  local j_body = {}      -- streamed body lines ("- path: summary")
  local j_subject = nil  -- subject line (last non-bullet stdout line)
  local j_total = 0      -- total file count from stderr
  local j_done = 0       -- files summarised so far

  -- Stdout: body lines streamed one-per-file, subject line last.
  local function on_stdout_line(line)
    if line == "" then return end
    if line:match("^%- ") then
      j_body[#j_body + 1] = line
    else
      j_subject = line
    end
  end

  -- Stderr: drives Noice progress messages with file counts and names.
  local function on_stderr_line(line)
    if line == "" then return end

    local n = line:match("^summarizing (%d+) file")
    if n then
      j_total = tonumber(n) or 0
      vim.schedule(function()
        emit_progress("report", token, "git-commit-ai",
          ("0/%d files"):format(j_total))
      end)
      return
    end

    local path_done = line:match("^  (.-)… .+$")
    if path_done then
      j_done = j_done + 1
      vim.schedule(function()
        emit_progress("report", token, "git-commit-ai",
          ("%d/%d %s"):format(j_done, j_total, path_done))
      end)
      return
    end

    if line:match("^generating subject") then
      vim.schedule(function()
        emit_progress("report", token, "git-commit-ai", "generating subject…")
      end)
    end
  end

  local feed_stdout, _stdout_raw = make_feeder(on_stdout_line)
  local feed_stderr, _stderr_raw = make_feeder(on_stderr_line)

  -- Abort on any user edit while generating
  local abort_group = "GitCommitAIAbort" .. bufnr
  vim.api.nvim_create_augroup(abort_group, { clear = true })
  vim.api.nvim_create_autocmd({ "InsertCharPre", "TextChanged" }, {
    group = abort_group,
    buffer = bufnr,
    once = true,
    -- Defer to avoid destroying the augroup from inside its own callback.
    callback = function() vim.schedule(function() clear(bufnr) end) end,
  })

  local cmd = { cfg.bin, "--dry-run" }
  if cfg.provider then vim.list_extend(cmd, { "--provider", cfg.provider }) end
  if cfg.model then vim.list_extend(cmd, { "--model", cfg.model }) end
  if cfg.context then vim.list_extend(cmd, { "--context", cfg.context }) end

  local j_err = {} -- all stderr lines for error reporting

  local my_job
  my_job = vim.fn.jobstart(cmd, {
    stdout_buffered = false,
    stderr_buffered = false,

    on_stdout = function(_, data)
      if data then feed_stdout(data) end
    end,

    on_stderr = function(_, data)
      if data then
        -- Capture all stderr for potential error display
        for _, l in ipairs(data) do
          if l ~= "" then j_err[#j_err + 1] = l end
        end
        feed_stderr(data)
      end
    end,

    on_exit = function(_, code)
      if gens[bufnr] ~= my_gen then return end -- cancelled / superseded

      -- Flush any partial line remaining in each accumulator
      if _stdout_raw[1] ~= "" then on_stdout_line(_stdout_raw[1]) end
      if _stderr_raw[1] ~= "" then on_stderr_line(_stderr_raw[1]) end

      vim.schedule(function()
        if gens[bufnr] ~= my_gen then return end

        jobs[bufnr] = nil
        pcall(vim.api.nvim_buf_del_extmark, bufnr, ns, marks[bufnr])
        marks[bufnr] = nil
        pcall(vim.api.nvim_del_augroup_by_name, abort_group)

        emit_progress("end", token, "git-commit-ai", nil)
        tokens[bufnr] = nil
        vim.defer_fn(release_fake_client, 200)

        if code ~= 0 then
          local msg = table.concat(j_err, "\n")
          if msg ~= "" then
            vim.notify("git-commit-ai: " .. msg, vim.log.levels.WARN)
          end
          return
        end

        if j_subject and j_subject ~= "" then
          insert_message(bufnr, j_subject, j_body)
        end
      end)
    end,
  })

  if my_job <= 0 then
    clear(bufnr)
    vim.notify(
      ("git-commit-ai: could not start '%s' — is it in PATH?"):format(cfg.bin),
      vim.log.levels.ERROR
    )
    return
  end

  jobs[bufnr] = my_job
end

-- ── Setup ─────────────────────────────────────────────────────────────────────

function M.setup(opts)
  cfg = vim.tbl_deep_extend("force", defaults, opts or {})

  local group = vim.api.nvim_create_augroup("GitCommitAI", { clear = true })

  vim.api.nvim_create_autocmd("FileType", {
    group = group,
    pattern = "gitcommit",
    callback = function(ev)
      local bufnr = ev.buf
      if has_user_content(vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)) then
        return
      end

      M.generate(bufnr)

      if cfg.keymap then
        vim.keymap.set("n", cfg.keymap, function()
          M.generate(bufnr)
        end, { buffer = bufnr, desc = "git-commit-ai: regenerate message" })
      end

      vim.api.nvim_create_autocmd("BufDelete", {
        group = group,
        buffer = bufnr,
        once = true,
        callback = function() clear(bufnr) end,
      })
    end,
  })
end

return M
