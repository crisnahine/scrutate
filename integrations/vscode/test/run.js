// Starts a real VS Code with this extension on a small repository and runs test/suite.js in it.
//   SCRUTATE_BIN=/path/to/scrutate node test/run.js     (no display: xvfb-run -a node test/run.js)
const cp = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");
const { runTests } = require("@vscode/test-electron");

(async () => {
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "scrutate-vscode-"));
  const repo = path.join(work, "repo");
  cp.execFileSync("bash", [path.join(__dirname, "..", "..", "fixture.sh"), repo]);
  // `scrutate.path` is a machine setting: a repository cannot name the program to run
  const user = path.join(work, "user-data", "User");
  fs.mkdirSync(user, { recursive: true });
  fs.writeFileSync(path.join(user, "settings.json"), JSON.stringify({ "scrutate.path": process.env.SCRUTATE_BIN || "scrutate" }));
  try {
    await runTests({
      extensionDevelopmentPath: path.join(__dirname, ".."),
      extensionTestsPath: path.join(__dirname, "suite.js"),
      launchArgs: [repo, "--user-data-dir", path.join(work, "user-data"), "--disable-extensions", "--disable-workspace-trust", "--no-sandbox", "--disable-gpu"],
    });
  } catch (e) {
    console.error(e);
    process.exit(1);
  }
})();
