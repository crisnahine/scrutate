// Scrutate for VS Code: a thin client over the CLI's JSON output. No bundler, no dependencies:
// package with `npx @vscode/vsce package` in this directory and install the .vsix.
const vscode = require("vscode");
const { execFile } = require("child_process");

function root() {
  const f = vscode.workspace.workspaceFolders;
  return f && f.length ? f[0].uri.fsPath : undefined;
}

function run(args) {
  const bin = vscode.workspace.getConfiguration("scrutate").get("path", "scrutate");
  return new Promise((resolve, reject) => {
    execFile(bin, args, { cwd: root(), maxBuffer: 64 * 1024 * 1024 }, (err, stdout, stderr) => {
      // exit 1 = no results, which is not an error
      if (err && err.code !== 1) return reject(new Error(stderr || err.message));
      resolve(stdout);
    });
  });
}

async function open(path, line) {
  const uri = vscode.Uri.joinPath(vscode.workspace.workspaceFolders[0].uri, path);
  const doc = await vscode.workspace.openTextDocument(uri);
  const ed = await vscode.window.showTextDocument(doc);
  const pos = new vscode.Position(Math.max(0, line - 1), 0);
  ed.selection = new vscode.Selection(pos, pos);
  ed.revealRange(new vscode.Range(pos, pos), vscode.TextEditorRevealType.InCenter);
}

async function pickHits(hits, placeHolder) {
  if (!hits.length) return vscode.window.showInformationMessage("Scrutate: no results");
  const items = hits.map((h) => ({
    label: `${h.kind ? "[" + h.kind + "] " : ""}${h.qualname || h.name || ""}`,
    description: `${h.path}:${h.start}`,
    detail: h.why ? Object.entries(h.why).map(([k, v]) => `${k} ${v}`).join("  ") : undefined,
    hit: h,
  }));
  const pick = await vscode.window.showQuickPick(items, { placeHolder, matchOnDescription: true });
  if (pick) await open(pick.hit.path, pick.hit.start);
}

function wordUnderCursor() {
  const ed = vscode.window.activeTextEditor;
  if (!ed) return undefined;
  const range = ed.document.getWordRangeAtPosition(ed.selection.active, /[A-Za-z_][A-Za-z0-9_.]*/);
  return range ? ed.document.getText(range) : undefined;
}

async function withErrors(f) {
  try {
    await f();
  } catch (e) {
    vscode.window.showErrorMessage(`Scrutate: ${e.message}`);
  }
}

function activate(context) {
  const k = () => vscode.workspace.getConfiguration("scrutate").get("results", 15);
  context.subscriptions.push(
    vscode.commands.registerCommand("scrutate.search", () =>
      withErrors(async () => {
        const q = await vscode.window.showInputBox({ prompt: "Scrutate search (meaning, name or identifier)" });
        if (!q) return;
        const out = await run(["search", q, "-k", String(k()), "--json"]);
        await pickHits(JSON.parse(out || "[]"), q);
      })
    ),
    vscode.commands.registerCommand("scrutate.grep", () =>
      withErrors(async () => {
        const q = await vscode.window.showInputBox({ prompt: "Scrutate grep (regex; instant, no index)" });
        if (!q) return;
        const out = await run(["grep", "--json", "--max-results", "500", "--sort", q, "."]);
        const items = out
          .split("\n")
          .filter((l) => l.trim())
          .map((l) => JSON.parse(l))
          .filter((r) => r.type === "match")
          .map((r) => ({ label: `${r.path}:${r.line}`, description: r.text.trim(), r }));
        if (!items.length) return vscode.window.showInformationMessage("Scrutate: no matches");
        const pick = await vscode.window.showQuickPick(items, { matchOnDescription: true });
        if (pick) await open(pick.r.path, pick.r.line);
      })
    ),
    vscode.commands.registerCommand("scrutate.symbol", () =>
      withErrors(async () => {
        const name = wordUnderCursor() || (await vscode.window.showInputBox({ prompt: "Symbol (Class.method works)" }));
        if (!name) return;
        await pickHits(JSON.parse((await run(["symbol", name, "--json"])) || "[]"), `definitions of ${name}`);
      })
    ),
    vscode.commands.registerCommand("scrutate.callers", () =>
      withErrors(async () => {
        const name = wordUnderCursor() || (await vscode.window.showInputBox({ prompt: "Function name" }));
        if (!name) return;
        await pickHits(JSON.parse((await run(["callers", name, "--json"])) || "[]"), `callers of ${name}`);
      })
    ),
    vscode.commands.registerCommand("scrutate.refs", () =>
      withErrors(async () => {
        const name = wordUnderCursor();
        if (!name) return;
        const r = JSON.parse((await run(["refs", name, "--json"])) || "{}");
        const items = [];
        for (const d of r.definitions || []) items.push({ label: `[def] ${d.qualname}`, description: `${d.path}:${d.start}`, path: d.path, line: d.start });
        for (const g of r.references || []) for (const l of g.lines) items.push({ label: `${g.symbol ? g.symbol.qualname : "(file)"}`, description: `${g.path}:${l.line}  ${l.text.trim()}`, path: g.path, line: l.line });
        if (!items.length) return vscode.window.showInformationMessage("Scrutate: no references");
        const pick = await vscode.window.showQuickPick(items, { matchOnDescription: true });
        if (pick) await open(pick.path, pick.line);
      })
    ),
    vscode.commands.registerCommand("scrutate.index", () =>
      withErrors(async () => {
        const out = await run(["index"]);
        vscode.window.showInformationMessage(`Scrutate: ${out.trim()}`);
      })
    )
  );
}

function deactivate() {}

module.exports = { activate, deactivate };
