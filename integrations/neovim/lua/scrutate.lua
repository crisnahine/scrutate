-- Scrutate for Neovim: ranked search, grep, symbols and callers through the CLI's JSON output,
-- delivered to the quickfix list. Install by adding this directory to 'runtimepath' (or with any
-- plugin manager pointing at integrations/neovim) and calling require("scrutate").setup().
local M = {}
M.config = { bin = "scrutate", results = 15 }

local function run(args)
  local cmd = { M.config.bin }
  vim.list_extend(cmd, args)
  -- stdout alone: scrutate reports index updates on stderr
  local ok, r = pcall(function() return vim.system(cmd, { text = true }):wait() end)
  if not ok then
    vim.notify("scrutate: " .. tostring(r), vim.log.levels.ERROR)
    return nil
  end
  if r.code ~= 0 and r.code ~= 1 then
    vim.notify("scrutate: " .. vim.trim(r.stderr ~= "" and r.stderr or r.stdout), vim.log.levels.ERROR)
    return nil
  end
  return r.stdout
end

local function to_qf(hits)
  local items = {}
  for _, h in ipairs(hits) do
    table.insert(items, { filename = h.path, lnum = h.start or h.line or 1, col = h.column or 1,
      text = (h.kind and ("[" .. h.kind .. "] ") or "") .. (h.qualname or h.name or h.text or "") })
  end
  vim.fn.setqflist({}, " ", { title = "scrutate", items = items })
  if #items > 0 then vim.cmd("copen") else vim.notify("scrutate: no results") end
end

function M.search(query)
  local out = run({ "search", "-k", tostring(M.config.results), "--json", "--", query })
  if out then to_qf(vim.json.decode(out ~= "" and out or "[]")) end
end

function M.grep(pattern)
  local out = run({ "grep", "--json", "--sort", "--max-results", "1000", "--", pattern, "." })
  if not out then return end
  local hits = {}
  for line in out:gmatch("[^\n]+") do
    local ok, r = pcall(vim.json.decode, line)
    if ok and r.type == "match" then table.insert(hits, { path = r.path, line = r.line, column = r.column, text = r.text }) end
  end
  to_qf(hits)
end

function M.symbol(name)
  local out = run({ "symbol", "--json", "--", name })
  if out then to_qf(vim.json.decode(out ~= "" and out or "[]")) end
end

function M.callers(name)
  local out = run({ "callers", "--json", "--", name })
  if out then to_qf(vim.json.decode(out ~= "" and out or "[]")) end
end

function M.setup(opts)
  M.config = vim.tbl_extend("force", M.config, opts or {})
  vim.api.nvim_create_user_command("Scrutate", function(a) M.search(a.args) end, { nargs = "+" })
  vim.api.nvim_create_user_command("ScrutateGrep", function(a) M.grep(a.args) end, { nargs = "+" })
  vim.api.nvim_create_user_command("ScrutateSymbol", function(a) M.symbol(a.args ~= "" and a.args or vim.fn.expand("<cword>")) end, { nargs = "?" })
  vim.api.nvim_create_user_command("ScrutateCallers", function(a) M.callers(a.args ~= "" and a.args or vim.fn.expand("<cword>")) end, { nargs = "?" })
end

return M
