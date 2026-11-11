# Scrutate for Neovim

```lua
-- lazy.nvim
{ dir = "/path/to/scrutate/integrations/neovim", config = function() require("scrutate").setup() end }
```

Commands: `:Scrutate <query>` (ranked search), `:ScrutateGrep <regex>`, `:ScrutateSymbol [name]`,
`:ScrutateCallers [name]` (default: word under cursor). Results go to the quickfix list.

The language server (definitions, references, symbols), on Neovim 0.11 or newer:

```lua
vim.lsp.config("scrutate", { cmd = { "scrutate", "lsp" }, root_markers = { ".git", ".scrutate" } })
vim.lsp.enable("scrutate")
```

Test: in a repository made by `integrations/fixture.sh`,
`nvim --headless -u NONE -l integrations/neovim/test/run.lua /path/to/scrutate` runs every
command and a go-to-definition through the language server. It last passed on Neovim 0.12.6
(2026-11-11).
