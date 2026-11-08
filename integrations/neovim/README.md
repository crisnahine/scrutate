# Scrutate for Neovim

```lua
-- lazy.nvim
{ dir = "/path/to/scrutate/integrations/neovim", config = function() require("scrutate").setup() end }
```

Commands: `:Scrutate <query>` (ranked search), `:ScrutateGrep <regex>`, `:ScrutateSymbol [name]`,
`:ScrutateCallers [name]` (default: word under cursor). Results go to the quickfix list.

Status: written against the CLI's JSON interface; not yet exercised in a running Neovim in CI.
