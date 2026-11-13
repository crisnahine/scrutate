<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="../../docs/assets/banner-jetbrains.svg" alt="JetBrains IDEs. How do I run scrutate from a JetBrains IDE?" width="880">
</p>

# Scrutate in JetBrains IDEs

[Documentation](../../docs/README.md) &nbsp;·&nbsp; Previous: [Neovim](../neovim/README.md) &nbsp;·&nbsp; Next: [GitHub Actions](../github/README.md)

<!-- End of head. -->

0.1.0 has no dedicated plugin (IntelliJ Platform SDK, Gradle build). It ships an **External
Tools** definition, which every JetBrains IDE supports without a plugin:

1. Under *Settings → Tools → External Tools*, add the four tools below (the IDE has no import
   button there). The same definitions are in `External Tools.xml` next to this README: copied
   into the `tools` folder of the IDE's configuration directory, they are read at the IDE's next
   start (not tried in an IDE).
2. Assign keyboard shortcuts under *Settings → Keymap → External Tools*.

| Tool | Program | Arguments | Working directory |
|---|---|---|---|
| Scrutate: search | `scrutate` | `search -k 15 -- "$Prompt$"` | `$ProjectFileDir$` |
| Scrutate: grep | `scrutate` | `grep --sort -- "$Prompt$" "$ProjectFileDir$"` | `$ProjectFileDir$` |
| Scrutate: symbol | `scrutate` | `symbol -- "$SelectedText$"` | `$ProjectFileDir$` |
| Scrutate: callers | `scrutate` | `callers -- "$SelectedText$"` | `$ProjectFileDir$` |

The `--` keeps a query that starts with `-` from being read as an option. Output appears in the
Run tool window. The IDE links `path:line` only for an absolute path, so the grep tool is given
the project directory and its results are links. The other three answer from the index with
repository-relative paths, which the IDE does not link; the language server below navigates
those.

## Language server through LSP4IJ

The [LSP4IJ](https://plugins.jetbrains.com/plugin/23257-lsp4ij) plugin (Red Hat, free) connects
any JetBrains IDE to a language server, so `scrutate lsp` gives Go to Declaration, Find Usages,
Go to Symbol and the Structure view from the Scrutate index.

1. Install LSP4IJ from *Settings → Plugins → Marketplace* and restart the IDE.
2. Open *Settings → Languages & Frameworks → Language Servers* and click **+**.
3. On the **Server** tab: name `Scrutate`, command `scrutate lsp` (use the full path to the
   binary if it is not on the IDE's `PATH`).
4. On the **Mappings** tab, under **File name patterns**, add the files to answer for, such as
   `*.py`, `*.rs`, `*.go`, `*.ts`, each with a language id (`python`, `rust`, ...). The server
   ignores the id; any value works.
5. Click **OK** and open a project file. The server takes the project directory from the IDE
   (`rootUri`); the first start builds `.scrutate/` and says so in the *Language Servers* tool
   window log.

The server answers definitions, references, workspace symbols and document symbols. It offers no
completion, hover or diagnostics, so the IDE keeps its own. Saving a file updates the index in the
background.

## MCP

The MCP server (`scrutate serve`) also works with the MCP settings of JetBrains AI Assistant and
Junie; see ../mcp/README.md.
