-- Runs the plugin in a headless Neovim against a real scrutate binary:
--   cd <a repository> && nvim --headless -u NONE -l <this file> <path to scrutate>
-- The repository must be the one integrations/fixture.sh makes (two Python files). Exits 1 on a failure.
local here = debug.getinfo(1, "S").source:sub(2):match("(.*/)")
vim.opt.runtimepath:append(here .. "..")
local bin = arg[1] or "scrutate"
require("scrutate").setup({ bin = bin })

-- a repository top, so the CLI run in a subdirectory finds it
vim.fn.mkdir(".git", "p")

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

-- Windows: a bare name comes from an absolute PATH entry, never from the working directory
local S = require("scrutate")
local there = { ["D:\\bin\\scrutate.EXE"] = true, ["scrutate.EXE"] = true, [".\\scrutate.EXE"] = true, ["rel\\scrutate.EXE"] = true }
local function exists(p) return there[p] == true end
check("a bare name is found on an absolute PATH entry", S._resolve("scrutate", true, "C:\\tools;;.;rel;D:\\bin\\", ".COM;.EXE", exists) == "D:\\bin\\scrutate.EXE",
  S._resolve("scrutate", true, "C:\\tools;;.;rel;D:\\bin\\", ".COM;.EXE", exists))
check("a name with its extension is found the same way", S._resolve("scrutate.EXE", true, "D:\\bin", ".COM;.EXE", exists) == "D:\\bin\\scrutate.EXE", nil)
check("a name that is only in the working directory is not run", S._resolve("scrutate", true, ".;rel", ".COM;.EXE", exists) == nil, S._resolve("scrutate", true, ".;rel", ".COM;.EXE", exists))
check("a path is taken as given", S._resolve("C:\\x\\scrutate.exe", true, "D:\\bin", ".EXE", exists) == "C:\\x\\scrutate.exe", nil)
check("elsewhere the system looks on PATH only", S._resolve("scrutate", false, "", "", exists) == "scrutate", nil)
check("the plugin's program is the one given", S.bin() == bin, S.bin())

-- the first command builds the index and says so on stderr, which must stay out of the JSON
local ok, e = pcall(vim.cmd, "ScrutateSymbol make_point")
local q = quickfix()
check(":ScrutateSymbol works before any index", ok and #q == 1 and q[1].file == "src/geo.py" and q[1].line == 6, ok and q or e)

vim.fn.system({ bin, "index" })
check("index builds", vim.v.shell_error == 0, vim.v.shell_error)

vim.cmd("Scrutate build the origin point")
q = quickfix()
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

vim.cmd([[ScrutateGrep -?def main\(]])
q = quickfix()
check("a pattern that starts with - is not read as an option", #q == 1 and has(q, "src/app.py", 4), q)

vim.cmd("ScrutateGrep zzzzqqqq_no_such_thing_anywhere")
check("no match leaves an empty list", #quickfix() == 0, quickfix())

-- an edit since the last index: the search updates the index first and reports it on stderr
vim.fn.writefile({ "def fresh_one():", "    return 2" }, "src/extra.py")
ok, e = pcall(vim.cmd, "Scrutate fresh_one")
q = quickfix()
check(":Scrutate after an edit, without indexing", ok and has(q, "src/extra.py", 1), ok and q or e)

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

-- from a folder below the top: answers name files from the top, and each one opens from here
vim.cmd("cd src")
-- no buffer left from the runs above: each name is resolved from here
vim.cmd("silent! %bwipeout!")
local function opens(list, tail, line)
  for _, e in ipairs(list) do
    if e.line == line and vim.fn.filereadable(e.file) == 1 and vim.fn.fnamemodify(e.file, ":p"):sub(-#tail) == tail then return true end
  end
  return false
end
for _, c in ipairs({
  { "ScrutateSymbol make_point", "src/geo.py", 6 },
  { "Scrutate build the origin point", "src/geo.py", 6 },
  { "ScrutateCallers make_point", "src/app.py", 4 },
  { [[ScrutateGrep def use_it]], "src/geo.py", 11 },
}) do
  ok, e = pcall(vim.cmd, c[1])
  q = quickfix()
  check(":" .. c[1] .. " from a subdirectory", ok and opens(q, c[2], c[3]), ok and q or e)
end
check("no index in the subdirectory", vim.fn.isdirectory(".scrutate") == 0, vim.fn.getcwd())
vim.cmd("cd ..")

-- qa! would end Neovim with status 0 whatever failed
vim.cmd(failed > 0 and "cquit 1" or "qa!")
