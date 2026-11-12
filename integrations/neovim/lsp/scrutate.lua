-- Neovim 0.11+ reads this for vim.lsp.enable("scrutate"). No filetypes: the server answers for every language.
-- The program is the plugin's `bin`, found on PATH by the plugin itself on Windows (see lua/scrutate.lua).
return {
  cmd = { require("scrutate").bin(), "lsp" },
  root_markers = { ".scrutate", ".git" },
}
