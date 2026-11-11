# Scrutate in JetBrains IDEs

A dedicated plugin (IntelliJ Platform SDK, Gradle build) is not part of 0.1.0. What ships is an
**External Tools** definition, which every JetBrains IDE supports without a plugin:

1. Under *Settings → Tools → External Tools*, add the four tools with the values below. The IDE
   has no import button there. The same definitions are in `External Tools.xml` next to this
   README: copied into the `tools` folder of the IDE's configuration directory, the IDE reads
   them at its next start (this copy was not tried in an IDE).
2. Assign keyboard shortcuts under *Settings → Keymap → External Tools*.

| Tool | Program | Arguments | Working directory |
|---|---|---|---|
| Scrutate: search | `scrutate` | `search "$Prompt$" -k 15` | `$ProjectFileDir$` |
| Scrutate: grep | `scrutate` | `grep --sort "$Prompt$" "$ProjectFileDir$"` | `$ProjectFileDir$` |
| Scrutate: symbol | `scrutate` | `symbol "$SelectedText$"` | `$ProjectFileDir$` |
| Scrutate: callers | `scrutate` | `callers "$SelectedText$"` | `$ProjectFileDir$` |

Output appears in the Run tool window. The IDE turns `path:line` into a link only for an
absolute path, so the grep tool is given the project directory and its results are links. The
other three answer from the index with paths relative to the repository, which the IDE does not
link; the language server below gives navigation for those.

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
