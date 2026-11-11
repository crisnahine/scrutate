-- Neovim 0.11+ reads this for vim.lsp.enable("scrutate"). No filetypes: the server answers for every language.
return {
  cmd = { "scrutate", "lsp" },
  root_markers = { ".scrutate", ".git" },
}
