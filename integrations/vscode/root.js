// Where scrutate answers from. No `vscode` module here, so plain Node tests it (test/root.test.js).
const { execFile } = require("child_process");
const path = require("path");

/**
 * The repository top the CLI uses for its index when run in `folder` (`scrutate config --json`),
 * or `folder` itself when the binary cannot say.
 * @param {string} bin @param {string} folder @returns {Promise<string>}
 */
function findRoot(bin, folder) {
  return new Promise((resolve) => {
    execFile(bin, ["config", "--json"], { cwd: folder }, (err, stdout) => {
      let root;
      try {
        root = JSON.parse(stdout).root;
      } catch {}
      resolve(!err && typeof root === "string" && root ? root : folder);
    });
  });
}

/** The file an answer's `path` names: answers give paths from the root. @param {string} root @param {string} p */
function fileOf(root, p) {
  return path.resolve(root, p);
}

module.exports = { findRoot, fileOf };
