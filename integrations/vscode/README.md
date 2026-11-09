# Scrutate for VS Code

The extension starts `scrutate lsp` as a language server for the open folder, so **Go to
Definition**, **Find All References**, **Go to Symbol in Workspace** and the **Outline** view work
in every language Scrutate indexes. The binary must be on `PATH`, or set `scrutate.path`. The
first start builds the index (`.scrutate/`); the server's log is in *Output → Scrutate*.

Commands (Command Palette): **Scrutate: Search**, **Grep**, **Go to symbol definition**,
**Who calls this?**, **References**, **Update index**. Results open in a Quick Pick and jump to
the location.

Build: `npm install` in this directory (`npm run check` type-checks `extension.js`), then
`npx @vscode/vsce package` and *Extensions → Install from VSIX*.

Status: the extension type-checks and packages; it has not yet been exercised inside a running
VS Code in this project's CI (see docs/TRACEABILITY.md). The server itself is tested end to end in
`crates/scrutate/tests/lsp.rs`.
