// Where scrutate answers from. No `vscode` module here, so plain Node tests it (test/root.test.js).
const { execFile } = require("child_process");
const fs = require("fs");
const path = require("path");

/**
 * The program to run for the `scrutate.path` setting, or undefined when there is none. On Windows
 * a bare name is looked up on PATH here: the system looks in the working directory first, and
 * that is the opened folder, so a `scrutate.exe` in a repository would run.
 * @param {string} setting
 * @param {{env?: NodeJS.ProcessEnv, platform?: string, exists?: (p: string) => boolean}} [on]
 * @returns {string | undefined}
 */
function resolveBinary(setting, on = {}) {
  const { env = process.env, platform = process.platform, exists = fs.existsSync } = on;
  if (platform !== "win32" || /[\\/]/.test(setting)) return setting;
  const exts = (env.PATHEXT || ".COM;.EXE;.BAT;.CMD").split(";").filter(Boolean);
  const names = path.win32.extname(setting) ? [setting] : exts.map((e) => setting + e);
  for (const dir of (env.PATH || env.Path || "").split(";")) {
    // an empty or relative entry would be the working directory again
    if (!path.win32.isAbsolute(dir)) continue;
    for (const name of names) {
      const full = path.win32.join(dir, name);
      if (exists(full)) return full;
    }
  }
  return undefined;
}

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

module.exports = { findRoot, fileOf, resolveBinary };
