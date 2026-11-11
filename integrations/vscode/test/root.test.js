// The root logic without VS Code: SCRUTATE_BIN=/path/to/scrutate node test/root.test.js
const assert = require("assert");
const cp = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");
const { findRoot, fileOf } = require("../root");

(async () => {
  const bin = process.env.SCRUTATE_BIN || "scrutate";
  const work = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "scrutate-vscode-root-")));
  try {
    const repo = path.join(work, "repo");
    cp.execFileSync("bash", [path.join(__dirname, "..", "..", "fixture.sh"), repo]);
    fs.mkdirSync(path.join(repo, ".git"));
    const sub = path.join(repo, "src");

    const root = await findRoot(bin, sub);
    assert.strictEqual(fs.realpathSync(root), repo, "a folder below the top answers from the top");
    const hits = JSON.parse(cp.execFileSync(bin, ["symbol", "--json", "--", "make_point"], { cwd: root, stdio: ["ignore", "pipe", "ignore"] }).toString());
    assert.strictEqual(hits.length, 1, JSON.stringify(hits));
    assert.ok(fs.existsSync(fileOf(root, hits[0].path)), hits[0].path);
    assert.ok(!fs.existsSync(path.join(sub, ".scrutate")), "no index in the folder");

    assert.strictEqual(await findRoot(path.join(work, "no-such-binary"), sub), sub, "without an answer, the folder");
    console.log("ok   root.test.js");
  } finally {
    fs.rmSync(work, { recursive: true, force: true });
  }
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
