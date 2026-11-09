# Scrutate in JetBrains IDEs

A dedicated plugin (IntelliJ Platform SDK, Gradle build) is not part of 0.1.0. What ships is an
**External Tools** definition, which every JetBrains IDE supports without a plugin:

1. *Settings → Tools → External Tools → Import* the file `External Tools.xml` next to this README
   (or create the tools by hand with the values below).
2. Assign keyboard shortcuts under *Settings → Keymap → External Tools*.

| Tool | Program | Arguments | Working directory |
|---|---|---|---|
| Scrutate: search | `scrutate` | `search "$Prompt$" -k 15` | `$ProjectFileDir$` |
| Scrutate: grep | `scrutate` | `grep --sort "$Prompt$" .` | `$ProjectFileDir$` |
| Scrutate: symbol | `scrutate` | `symbol "$SelectedText$"` | `$ProjectFileDir$` |
| Scrutate: callers | `scrutate` | `callers "$SelectedText$"` | `$ProjectFileDir$` |

Output appears in the Run tool window; `path:line` references are clickable because the
IDE recognises the `file:line` output filter configured in the XML.

## Language server through LSP4IJ

The [LSP4IJ](https://plugins.jetbrains.com/plugin/23257-lsp4ij) plugin (Red Hat, free) connects
any JetBrains IDE to a language server. With it, `scrutate lsp` gives Go to Declaration, Find
Usages, Go to Symbol and the Structure view from the Scrutate index.

1. Install LSP4IJ from *Settings → Plugins → Marketplace* and restart the IDE.
2. Open *Settings → Languages & Frameworks → Language Servers* and click **+**.
3. On the **Server** tab: name `Scrutate`, command `scrutate lsp` (use the full path to the
   binary if it is not on the IDE's `PATH`).
4. On the **Mappings** tab, under **File name patterns**, add the files the server should answer
   for, for example `*.py`, `*.rs`, `*.go`, `*.ts`, with a language id (`python`, `rust`, ...).
   The server does not use the language id; any value works.
5. Click **OK**, then open a file of the project. The server takes the project directory from the
   IDE (`rootUri`); the first start builds `.scrutate/` and says so in the
   *Language Servers* tool window log.

The server answers definitions, references, workspace symbols and document symbols. It offers no
completion, hover or diagnostics, so the IDE's own features for those stay as they are. Saving a
file updates the index in the background.

## MCP

Alternatively, the MCP server (`scrutate serve`) works with JetBrains AI Assistant / Junie MCP
settings; see ../mcp/README.md.
