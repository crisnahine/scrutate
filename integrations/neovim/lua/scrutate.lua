-- Scrutate for Neovim: ranked search, grep, symbols and callers through the CLI's JSON output,
-- delivered to the quickfix list. Install by adding this directory to 'runtimepath' (or with any
-- plugin manager pointing at integrations/neovim) and calling require("scrutate").setup().
local M = {}
M.config = { bin = "scrutate", results = 15 }

-- `bin` as the program to start, or nil when there is none. On Windows a bare name is looked up
-- on PATH here: the system looks in the new process's directory first, and that is the opened
-- repository, so a scrutate.exe placed in one would run.
function M._resolve(bin, windows, path, pathext, exists)
  if not windows or bin:find("[/\\]") then return bin end
  local names = {}
  if bin:match("%.[^.]+$") then
    names = { bin }
  else
    for ext in pathext:gmatch("[^;]+") do table.insert(names, bin .. ext) end
  end
  for dir in path:gmatch("[^;]+") do
    -- an entry may be quoted ("C:\\Program Files\\x")
    dir = (dir:gsub('^"(.*)"$', "%1"))
    -- an entry that is not absolute would be the working directory again
    if dir:match("^%a:[/\\]") or dir:match("^[/\\][/\\]") then
      for _, name in ipairs(names) do
        local full = dir:gsub("[/\\]+$", "") .. "\\" .. name
        if exists(full) then return full end
      end
    end
  end
  return nil
end

function M.bin()
  local found = M._resolve(M.config.bin, vim.fn.has("win32") == 1, vim.env.PATH or "", vim.env.PATHEXT or ".COM;.EXE;.BAT;.CMD",
    function(p) return (vim.uv or vim.loop).fs_stat(p) ~= nil end)
  if not found then
    error(M.config.bin .. " was not found on PATH: install scrutate, or call setup({ bin = [[C:\\path\\to\\scrutate.exe]] })", 0)
  end
  return found
end

-- The directory the CLI answers from (the repository top), so every path in an answer opens from it.
local function root()
  local cwd = vim.fn.getcwd()
  local ok, r = pcall(function() return vim.system({ M.bin(), "config", "--json" }, { text = true }):wait() end)
  if not ok or r.code ~= 0 then return cwd end
  local decoded, v = pcall(vim.json.decode, r.stdout)
  return decoded and type(v) == "table" and type(v.root) == "string" and v.root or cwd
end

local function run(args)
  local found, bin = pcall(M.bin)
  if not found then
    vim.notify("scrutate: " .. tostring(bin), vim.log.levels.ERROR)
    return nil
  end
  local cmd = { bin }
  vim.list_extend(cmd, args)
  local dir = root()
  -- stdout alone: scrutate reports index updates on stderr
  local ok, r = pcall(function() return vim.system(cmd, { text = true, cwd = dir }):wait() end)
  if not ok then
    vim.notify("scrutate: " .. tostring(r), vim.log.levels.ERROR)
    return nil
  end
  if r.code ~= 0 and r.code ~= 1 then
    vim.notify("scrutate: " .. vim.trim(r.stderr ~= "" and r.stderr or r.stdout), vim.log.levels.ERROR)
    return nil
  end
  return r.stdout, dir
end

local function to_qf(hits, dir)
  local items = {}
  for _, h in ipairs(hits) do
    table.insert(items, { filename = dir .. "/" .. h.path, lnum = h.start or h.line or 1, col = h.column or 1,
      text = (h.kind and ("[" .. h.kind .. "] ") or "") .. (h.qualname or h.name or h.text or "") })
  end
  vim.fn.setqflist({}, " ", { title = "scrutate", items = items })
  if #items > 0 then vim.cmd("copen") else vim.notify("scrutate: no results") end
end

function M.search(query)
  local out, dir = run({ "search", "-k", tostring(M.config.results), "--json", "--", query })
  if out then to_qf(out ~= "" and vim.json.decode(out).result or {}, dir) end
end

function M.grep(pattern)
  local out, dir = run({ "grep", "--json", "--sort", "--max-results", "1000", "--", pattern, "." })
  if not out then return end
  local hits = {}
  for line in out:gmatch("[^\n]+") do
    local ok, r = pcall(vim.json.decode, line)
    if ok and r.type == "match" then table.insert(hits, { path = r.path, line = r.line, column = r.column, text = r.text }) end
  end
  to_qf(hits, dir)
end

function M.symbol(name)
  local out, dir = run({ "symbol", "--json", "--", name })
  if out then to_qf(out ~= "" and vim.json.decode(out).result or {}, dir) end
end

function M.callers(name)
  local out, dir = run({ "callers", "--json", "--", name })
  if out then to_qf(out ~= "" and vim.json.decode(out).result or {}, dir) end
end

function M.setup(opts)
  M.config = vim.tbl_extend("force", M.config, opts or {})
  vim.api.nvim_create_user_command("Scrutate", function(a) M.search(a.args) end, { nargs = "+" })
  vim.api.nvim_create_user_command("ScrutateGrep", function(a) M.grep(a.args) end, { nargs = "+" })
  vim.api.nvim_create_user_command("ScrutateSymbol", function(a) M.symbol(a.args ~= "" and a.args or vim.fn.expand("<cword>")) end, { nargs = "?" })
  vim.api.nvim_create_user_command("ScrutateCallers", function(a) M.callers(a.args ~= "" and a.args or vim.fn.expand("<cword>")) end, { nargs = "?" })
end

return M
