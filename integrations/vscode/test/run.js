// Starts a real VS Code with this extension on a small repository and runs test/suite.js in it,
// then again on a folder below the top of a second one.
//   SCRUTATE_BIN=/path/to/scrutate node test/run.js     (no display: xvfb-run -a node test/run.js)
const cp = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");
const { runTests } = require("@vscode/test-electron");

(async () => {
  // the real path, as the CLI prints the root (macOS gives the temp directory through a link)
  const work = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "scrutate-vscode-")));
  const fixture = (name) => {
    const repo = path.join(work, name);
    cp.execFileSync("bash", [path.join(__dirname, "..", "..", "fixture.sh"), repo]);
    fs.mkdirSync(path.join(repo, ".git"));
    return repo;
  };
  // `scrutate.path` is a machine setting: a repository cannot name the program to run
  const user = path.join(work, "user-data", "User");
  fs.mkdirSync(user, { recursive: true });
  fs.writeFileSync(path.join(user, "settings.json"), JSON.stringify({ "scrutate.path": process.env.SCRUTATE_BIN || "scrutate" }));
  try {
    for (const [name, folder] of [["repo", ""], ["sub", "src"]]) {
      const repo = fixture(name);
      await runTests({
        extensionDevelopmentPath: path.join(__dirname, ".."),
        extensionTestsPath: path.join(__dirname, "suite.js"),
        extensionTestsEnv: { SCRUTATE_TEST_TOP: repo },
        launchArgs: [path.join(repo, folder), "--user-data-dir", path.join(work, "user-data"), "--disable-extensions", "--disable-workspace-trust", "--no-sandbox", "--disable-gpu"],
      });
    }
  } catch (e) {
    console.error(e);
    process.exit(1);
  }
})();
