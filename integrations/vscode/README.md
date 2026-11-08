# Scrutate for VS Code

A thin client over the `scrutate` CLI (the binary must be on `PATH`, or set `scrutate.path`).

Commands (Command Palette): **Scrutate: Search**, **Grep**, **Go to symbol definition**,
**Who calls this?**, **References**, **Update index**. Results open in a Quick Pick and jump to
the location.

Build: `npx @vscode/vsce package` in this directory, then *Extensions → Install from VSIX*.

Status: implemented against the CLI's JSON interface; it has not yet been exercised inside a running
VS Code in this project's CI (see docs/TRACEABILITY.md).
