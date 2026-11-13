// Starts a real VS Code with this extension on a small repository and runs test/suite.js in it,
// then again on a folder below the top of a second one.
//   SCRUTATE_BIN=/path/to/scrutate node test/run.js     (no display: xvfb-run -a node test/run.js)
// VSCODE_VERSION picks the VS Code (default stable). SCRUTATE_VSIX=/path/to/scrutate.vsix installs
// that package and tests the installed copy instead of this folder.
const cp = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");
const { runTests, runVSCodeCommand } = require("@vscode/test-electron");

(async () => {
  // the real path, as the CLI prints the root (macOS gives the temp directory through a link)
  const work = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "scrutate-vscode-")));
  const fixture = (name) => {
    const repo = path.join(work, name);
    cp.execFileSync("bash", [path.join(__dirname, "..", "..", "fixture.sh"), repo]);
    fs.mkdirSync(path.join(repo, ".git"));
    return repo;
  };
  // `scrutate.path` is a machine setting: a repository cannot name the program to run. Each launch
  // gets its own user data: VS Code 1.85 trips over the instance socket a first launch left, and on
  // macOS that socket path must stay under 104 bytes, so it lives under /tmp there
  const userData = (name) => {
    const dir = process.platform === "darwin" ? fs.mkdtempSync(`/tmp/scv-${name}-`) : path.join(work, `user-data-${name}`);
    fs.mkdirSync(path.join(dir, "User"), { recursive: true });
    fs.writeFileSync(path.join(dir, "User", "settings.json"), JSON.stringify({ "scrutate.path": process.env.SCRUTATE_BIN || "scrutate" }));
    return dir;
  };
  const version = process.env.VSCODE_VERSION || "stable";
  let extensionDevelopmentPath = path.join(__dirname, "..");
  let extensions = ["--disable-extensions"];
  let expected = extensionDevelopmentPath;
  let suite = path.join(__dirname, "suite.js");
  try {
    if (process.env.SCRUTATE_VSIX) {
      const dir = path.join(work, "extensions");
      await runVSCodeCommand(["--install-extension", process.env.SCRUTATE_VSIX, "--extensions-dir", dir], { version });
      // the installed copy runs as the development extension, with the suite copied into it: VS Code
      // gives a file the vscode API object of the extension whose folder holds it, so the suite's
      // stubs (input box, quick pick) reach the extension only from inside its folder
      const installed = fs.readdirSync(dir).find((d) => d.startsWith("scrutate.scrutate-"));
      if (!installed) throw new Error(`--install-extension left no scrutate.scrutate-* in ${dir}`);
      extensionDevelopmentPath = path.join(dir, installed);
      fs.mkdirSync(path.join(extensionDevelopmentPath, "test-run"), { recursive: true });
      fs.copyFileSync(path.join(__dirname, "suite.js"), path.join(extensionDevelopmentPath, "test-run", "suite.js"));
      suite = path.join(extensionDevelopmentPath, "test-run", "suite.js");
      extensions = ["--extensions-dir", dir, "--disable-extensions"];
      expected = extensionDevelopmentPath;
    }
    for (const [name, folder] of [["repo", ""], ["sub", "src"]]) {
      const repo = fixture(name);
      await runTests({
        version,
        extensionDevelopmentPath,
        extensionTestsPath: suite,
        extensionTestsEnv: { SCRUTATE_TEST_TOP: repo, SCRUTATE_TEST_EXTENSION_UNDER: expected },
        launchArgs: [path.join(repo, folder), "--user-data-dir", userData(name), ...extensions, "--disable-workspace-trust", "--no-sandbox", "--disable-gpu"],
      });
    }
  } catch (e) {
    console.error(e);
    process.exit(1);
  }
})();
