# Scrutate for VS Code

The extension starts `scrutate lsp` as a language server for the open folder, so **Go to
Definition**, **Find All References**, **Go to Symbol in Workspace** and the **Outline** view work
in every language Scrutate indexes. The binary must be on `PATH`, or set `scrutate.path` in your
user settings (it is a machine setting: a repository's own settings cannot name the program to
run). The first start builds the index (`.scrutate/`); the server's log is in *Output → Scrutate*.

Commands (Command Palette): **Scrutate: Search**, **Grep**, **Go to symbol definition**,
**Who calls this?**, **References**, **Update index**. Results open in a Quick Pick and jump to
the location.

Build: `npm install` in this directory (`npm run check` type-checks `extension.js`), then
`npx @vscode/vsce package` and *Extensions → Install from VSIX*.

Test: `SCRUTATE_BIN=/path/to/scrutate npm test` starts a real VS Code with the extension on a
small repository (`test/run.js`, with `@vscode/test-electron`) and checks the definition,
references, workspace symbol and outline providers and every command (`test/suite.js`). On a
machine without a display: `xvfb-run -a npm test`. It last passed in VS Code 1.145.3 on Linux
arm64 (2026-11-11). The server itself is also tested end to end in `crates/scrutate/tests/lsp.rs`.
