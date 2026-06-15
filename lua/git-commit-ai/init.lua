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
  -- Gate on the saved original, not the ref count: a release() defers its
  -- restore by 200ms, so a re-acquire inside that window still sees the patch
  -- installed. Patching again there would save the *patched* fn as the
  -- original and leak it permanently. Only patch when truly unpatched.
  if _orig_get_client == nil then
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

-- ── Shared progress ────────────────────────────────────────────────────────────
-- All buffers/jobs share a single, ref-counted progress entry. Noice keys a
-- progress line by client_id+token, so a per-buffer token shows one line *per
-- buffer* — and more than one gitcommit buffer (verbose commit, git plugins,
-- session restore) then yields duplicate spinners. One constant token + a ref
-- count guarantees exactly one line however many generations are in flight.

local PROG_TOKEN = "git-commit-ai"
local prog_refs = 0

local function progress_begin()
  prog_refs = prog_refs + 1
  if prog_refs == 1 then
    emit_progress("begin", PROG_TOKEN, "git-commit-ai", "starting…")
  end
end

local function progress_report(msg, model)
  if prog_refs > 0 then
    local display = model and ("[" .. model .. "] " .. msg) or msg
    emit_progress("report", PROG_TOKEN, "git-commit-ai", display)
  end
end

local function progress_end()
  if prog_refs <= 0 then return end
  prog_refs = prog_refs - 1
  if prog_refs == 0 then
    emit_progress("end", PROG_TOKEN, "git-commit-ai", nil)
  end
end

-- ── Per-buffer state ──────────────────────────────────────────────────────────

local jobs   = {} ---@type table<integer, integer>
local marks  = {} ---@type table<integer, integer>
local tokens = {} ---@type table<integer, boolean>  -- buffer holds a progress ref
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
  -- The job may finish after the user has already written/closed the commit,
  -- leaving the buffer gone or read-only. Don't error trying to write it.
  if not vim.api.nvim_buf_is_valid(bufnr) then return end
  if not vim.bo[bufnr].modifiable or not vim.api.nvim_buf_is_loaded(bufnr) then
    return
  end
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
    tokens[bufnr] = nil
    progress_end()
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

  local my_gen = (gens[bufnr] or 0) + 1
  gens[bufnr] = my_gen

  acquire_fake_client()
  tokens[bufnr] = true
  progress_begin()

  -- Defer one tick so the buffer is visible before the extmark appears.
  vim.schedule(function()
    if gens[bufnr] ~= my_gen then return end
    local hl = virt_hl()
    if hl then
      marks[bufnr] = vim.api.nvim_buf_set_extmark(bufnr, ns, 0, 0, {
        virt_text = { { "  generating commit message... type your own to cancel.", hl } },
        virt_text_pos = "eol",
      })
    end
  end)

  -- Per-job closure state
  local j_body = {}      -- collected body lines (formatted "- text")
  local j_subject = nil  -- subject line

  -- Stdout: NDJSON events, one per line.
  -- {"kind":"progress","msg":"..."}  — drives Noice progress display
  -- {"kind":"body","text":"..."}     — appended to commit body
  -- {"kind":"subject","text":"..."}  — becomes commit subject
  local function on_stdout_line(line)
    if line == "" then return end
    local ok, event = pcall(vim.json.decode, line)
    if not ok or type(event) ~= "table" then return end

    if event.kind == "progress" and type(event.msg) == "string" then
      local model = type(event.model) == "string" and event.model or nil
      vim.schedule(function()
        progress_report(event.msg, model)
      end)
    elseif event.kind == "body" and type(event.text) == "string" then
      j_body[#j_body + 1] = "- " .. event.text
    elseif event.kind == "subject" and type(event.text) == "string" then
      j_subject = event.text
    end
  end

  local feed_stdout, _stdout_raw = make_feeder(on_stdout_line)
  local j_err = {} -- stderr lines captured for error reporting on non-zero exit

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

  local my_job
  my_job = vim.fn.jobstart(cmd, {
    stdin = "null",
    stdout_buffered = false,
    stderr_buffered = false,

    on_stdout = function(_, data)
      if data then feed_stdout(data) end
    end,

    on_stderr = function(_, data)
      if data then
        for _, l in ipairs(data) do
          if l ~= "" then j_err[#j_err + 1] = l end
        end
      end
    end,

    on_exit = function(_, code)
      if gens[bufnr] ~= my_gen then return end -- cancelled / superseded

      -- Flush any partial line remaining in the stdout accumulator
      if _stdout_raw[1] ~= "" then on_stdout_line(_stdout_raw[1]) end

      vim.schedule(function()
        if gens[bufnr] ~= my_gen then return end

        jobs[bufnr] = nil
        pcall(vim.api.nvim_buf_del_extmark, bufnr, ns, marks[bufnr])
        marks[bufnr] = nil
        pcall(vim.api.nvim_del_augroup_by_name, abort_group)

        if tokens[bufnr] then
          tokens[bufnr] = nil
          progress_end()
          vim.defer_fn(release_fake_client, 200)
        end

        if code ~= 0 then
          local msg = table.concat(j_err, "\n")
          if msg ~= "" and not msg:match("no staged changes") then
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

-- Trigger generation once the UI is ready.
--
-- When `git commit` launches Neovim, the `FileType gitcommit` autocmd fires
-- during startup, *before* the first screen redraw. Kicking off generation
-- synchronously there (spawning the job, monkey-patching vim.lsp, emitting
-- Noice/LSP progress) delays that first paint and leaves the terminal blank.
--
-- So we defer: if Neovim hasn't finished entering, wait for VimEnter; otherwise
-- schedule onto the next loop tick. Either way the buffer is painted first.
-- Tracks buffers we've already auto-triggered, for this buffer's whole
-- lifetime (cleared on BufDelete). `FileType gitcommit` can fire repeatedly —
-- lazy.nvim's post-load re-fire, syntax/ftplugin, even after VimEnter — and
-- each `M.generate` ends any in-flight progress then begins a new one, which
-- shows up as the progress appearing twice. Auto-trigger at most once.
local triggered = {} ---@type table<integer, boolean>
local group ---@type integer  set in setup()

local function trigger(bufnr)
  if triggered[bufnr] then return end
  triggered[bufnr] = true

  local function start()
    -- Buffer may have been wiped, or content typed/filled, while we waited.
    if not vim.api.nvim_buf_is_valid(bufnr) then return end
    if has_user_content(vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)) then
      return
    end
    M.generate(bufnr)
  end

  if vim.v.vim_did_enter == 1 then
    vim.schedule(start)
  else
    vim.api.nvim_create_autocmd("VimEnter", {
      group = group,
      once = true,
      callback = function() vim.schedule(start) end,
    })
  end
end

function M.setup(opts)
  cfg = vim.tbl_deep_extend("force", defaults, opts or {})

  group = vim.api.nvim_create_augroup("GitCommitAI", { clear = true })

  vim.api.nvim_create_autocmd("FileType", {
    group = group,
    pattern = "gitcommit",
    callback = function(ev)
      local bufnr = ev.buf

      -- Only act on the real commit-message buffer. Plugins like committia.vim
      -- open extra `gitcommit`-filetype windows (a read-only status/diff
      -- preview) as scratch buffers; generating in those gave duplicate
      -- progress spinners and a "not modifiable" error. The actual
      -- COMMIT_EDITMSG is a normal, writable file buffer (empty buftype).
      local bo = vim.bo[bufnr]
      if bo.buftype ~= "" or not bo.modifiable then return end

      if has_user_content(vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)) then
        return
      end

      -- Keymap and cleanup are cheap and don't touch the UI, so wire them up
      -- now; the actual generation is deferred until the editor is drawn.
      if cfg.keymap then
        vim.keymap.set("n", cfg.keymap, function()
          M.generate(bufnr)
        end, { buffer = bufnr, desc = "git-commit-ai: regenerate message" })
      end

      vim.api.nvim_create_autocmd("BufDelete", {
        group = group,
        buffer = bufnr,
        once = true,
        callback = function()
          clear(bufnr)
          triggered[bufnr] = nil
        end,
      })

      trigger(bufnr)
    end,
  })
end

return M
