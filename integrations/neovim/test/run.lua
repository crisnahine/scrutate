-- Runs the plugin in a headless Neovim against a real scrutate binary:
--   cd <a repository> && nvim --headless -u NONE -l <this file> <path to scrutate>
-- The repository must be the one integrations/fixture.sh makes (two Python files). Exits 1 on a failure.
local here = debug.getinfo(1, "S").source:sub(2):match("(.*/)")
vim.opt.runtimepath:append(here .. "..")
local bin = arg[1] or "scrutate"
require("scrutate").setup({ bin = bin })

local failed = 0
local function check(what, ok, got)
  if not ok then
    failed = failed + 1
    io.stderr:write("FAIL " .. what .. ": " .. vim.inspect(got) .. "\n")
  else
    io.stdout:write("ok   " .. what .. "\n")
  end
end
local function quickfix()
  local out = {}
  for _, e in ipairs(vim.fn.getqflist()) do
    table.insert(out, { file = vim.fn.bufname(e.bufnr), line = e.lnum, text = e.text })
  end
  return out
end
local function has(list, file, line)
  for _, e in ipairs(list) do
    if e.file == file and e.line == line then return true end
  end
  return false
end

vim.fn.system({ bin, "index" })
check("index builds", vim.v.shell_error == 0, vim.v.shell_error)

vim.cmd("Scrutate build the origin point")
local q = quickfix()
check(":Scrutate finds make_point first", q[1] and q[1].file == "src/geo.py" and q[1].line == 6 and q[1].text:find("make_point", 1, true), q)

vim.cmd([[ScrutateGrep def \w+]])
q = quickfix()
check(":ScrutateGrep lists every def", #q == 4 and has(q, "src/geo.py", 2) and has(q, "src/app.py", 4), q)

vim.cmd("ScrutateSymbol Point.norm")
q = quickfix()
check(":ScrutateSymbol finds the method", #q == 1 and q[1].file == "src/geo.py" and q[1].line == 2, q)

vim.cmd("edit src/app.py")
vim.api.nvim_win_set_cursor(0, { 5, 13 })
vim.cmd("ScrutateCallers")
q = quickfix()
check(":ScrutateCallers takes the word under the cursor", #q == 2 and has(q, "src/app.py", 4) and has(q, "src/geo.py", 11), q)

vim.cmd("ScrutateGrep zzzzqqqq_no_such_thing_anywhere")
check("no match leaves an empty list", #quickfix() == 0, quickfix())

-- the language server, from the lsp/scrutate.lua the plugin ships, with this run's binary
if vim.lsp.config then
  vim.lsp.config("scrutate", { cmd = { bin, "lsp" } })
  check("the shipped config supplies the root markers", vim.deep_equal(vim.lsp.config["scrutate"].root_markers, { ".scrutate", ".git" }), vim.lsp.config["scrutate"])
  vim.lsp.enable("scrutate")
  vim.cmd("edit src/app.py")
  local attached = vim.wait(20000, function() return #vim.lsp.get_clients({ bufnr = 0, name = "scrutate" }) > 0 end, 50)
  check("scrutate lsp attaches", attached, vim.lsp.get_clients())
  if attached then
    local client = vim.lsp.get_clients({ bufnr = 0, name = "scrutate" })[1]
    local params = { textDocument = { uri = vim.uri_from_bufnr(0) }, position = { line = 4, character = 13 } }
    local found
    vim.wait(20000, function()
      local r = client:request_sync("textDocument/definition", params, 5000, 0)
      found = r and r.result
      return found ~= nil and #found > 0
    end, 200)
    local first = found and found[1]
    check("go to definition answers from the index", first and first.uri:match("src/geo.py$") and first.range.start.line == 5, found)
  end
else
  io.stdout:write("skip the language server check: this Neovim has no vim.lsp.config\n")
end

vim.cmd("qa" .. (failed > 0 and "!" or ""))
os.exit(failed > 0 and 1 or 0)
