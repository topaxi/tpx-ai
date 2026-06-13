local M = {}

---@class GitCommitAIConfig
---@field bin string        binary name or absolute path (must be in PATH if name only)
---@field provider string?  "anthropic" | "ollama" | nil (uses binary default)
---@field model string?     model name override, or nil to use binary default
---@field context string?   extra context passed via --context
---@field virtual_text boolean|string  false to disable, or highlight group name
---@field virtual_text_msg string      text shown while generating
---@field keymap string?   normal-mode key to re-trigger in gitcommit buffers (nil = off)

---@type GitCommitAIConfig
local defaults = {
  bin = "git-commit",
  provider = nil,
  model = nil,
  context = nil,
  virtual_text = "Comment",
  virtual_text_msg = "  generating…",
  keymap = "<leader>gc",
}

local ns = vim.api.nvim_create_namespace("git_commit_ai")

-- per-buffer state
local jobs = {}   ---@type table<integer, integer>   bufnr -> job_id
local marks = {}  ---@type table<integer, integer>   bufnr -> extmark_id

local cfg = {}  ---@type GitCommitAIConfig

-- cancel any running job and remove the indicator extmark for bufnr
local function cancel(bufnr)
  if jobs[bufnr] then
    vim.fn.jobstop(jobs[bufnr])
    jobs[bufnr] = nil
  end
  if marks[bufnr] then
    pcall(vim.api.nvim_buf_del_extmark, bufnr, ns, marks[bufnr])
    marks[bufnr] = nil
  end
  pcall(vim.api.nvim_del_augroup_by_name, "GitCommitAIAbort" .. bufnr)
end

-- true when there is already non-comment, non-empty content at the top of the buffer
local function has_user_content(lines)
  for _, l in ipairs(lines) do
    if l:match("^#") then break end
    if l ~= "" then return true end
  end
  return false
end

local function insert_message(bufnr, message)
  if not vim.api.nvim_buf_is_valid(bufnr) then return end
  local lines = vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)
  if has_user_content(lines) then return end

  -- find where the git comment block starts
  local first_comment = #lines + 1
  for i, l in ipairs(lines) do
    if l:match("^#") then
      first_comment = i
      break
    end
  end

  local msg = vim.split(vim.trim(message), "\n")
  -- assemble: [message lines] [blank separator] [original comment lines]
  local new = {}
  vim.list_extend(new, msg)
  new[#new + 1] = ""
  for i = first_comment, #lines do
    new[#new + 1] = lines[i]
  end

  vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, new)

  local winid = vim.fn.bufwinid(bufnr)
  if winid ~= -1 then
    pcall(vim.api.nvim_win_set_cursor, winid, { 1, #msg[1] })
  end
end

--- Kick off AI generation for bufnr. Called automatically on FileType gitcommit
--- and can be called manually (e.g. from a keymap).
---@param bufnr integer?
function M.generate(bufnr)
  bufnr = bufnr or vim.api.nvim_get_current_buf()
  cancel(bufnr)

  local hl = type(cfg.virtual_text) == "string" and cfg.virtual_text
    or (cfg.virtual_text and "Comment" or nil)

  if hl then
    marks[bufnr] = vim.api.nvim_buf_set_extmark(bufnr, ns, 0, 0, {
      virt_text = { { cfg.virtual_text_msg, hl } },
      virt_text_pos = "eol",
    })
  end

  local cmd = { cfg.bin, "--dry-run" }
  if cfg.provider then vim.list_extend(cmd, { "--provider", cfg.provider }) end
  if cfg.model then vim.list_extend(cmd, { "--model", cfg.model }) end
  if cfg.context then vim.list_extend(cmd, { "--context", cfg.context }) end

  -- register abort listeners: any text change cancels in-flight generation
  local abort_group = "GitCommitAIAbort" .. bufnr
  vim.api.nvim_create_augroup(abort_group, { clear = true })
  vim.api.nvim_create_autocmd({ "InsertCharPre", "TextChanged" }, {
    group = abort_group,
    buffer = bufnr,
    once = true,
    callback = function() cancel(bufnr) end,
  })

  local out = {}
  local err = {}

  local job = vim.fn.jobstart(cmd, {
    stdout_buffered = true,
    stderr_buffered = true,
    on_stdout = function(_, data) out = data end,
    on_stderr = function(_, data) err = data end,
    on_exit = function(_, code)
      jobs[bufnr] = nil
      pcall(vim.api.nvim_buf_del_extmark, bufnr, ns, marks[bufnr])
      marks[bufnr] = nil
      pcall(vim.api.nvim_del_augroup_by_name, abort_group)

      if code ~= 0 then
        local msg = table.concat(
          vim.tbl_filter(function(l) return l ~= "" end, err),
          "\n"
        )
        if msg ~= "" then
          vim.schedule(function()
            vim.notify("git-commit-ai: " .. msg, vim.log.levels.WARN)
          end)
        end
        return
      end

      -- jobstart appends a trailing "" entry; strip it
      while #out > 0 and out[#out] == "" do out[#out] = nil end
      local message = table.concat(out, "\n")
      if message == "" then return end

      vim.schedule(function() insert_message(bufnr, message) end)
    end,
  })

  if job <= 0 then
    cancel(bufnr)
    vim.notify(
      ("git-commit-ai: could not start '%s' — is it in PATH?"):format(cfg.bin),
      vim.log.levels.ERROR
    )
    return
  end

  jobs[bufnr] = job
end

--- Configure and activate the plugin.
---@param opts GitCommitAIConfig?
function M.setup(opts)
  cfg = vim.tbl_deep_extend("force", defaults, opts or {})

  local group = vim.api.nvim_create_augroup("GitCommitAI", { clear = true })

  vim.api.nvim_create_autocmd("FileType", {
    group = group,
    pattern = "gitcommit",
    callback = function(ev)
      local bufnr = ev.buf

      -- skip if the commit was pre-populated (--fixup, -m, amend with content, etc.)
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
        callback = function() cancel(bufnr) end,
      })
    end,
  })
end

return M
