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

Alternatively, the MCP server (`scrutate serve`) works with JetBrains AI Assistant / Junie MCP
settings; see ../mcp/README.md.
