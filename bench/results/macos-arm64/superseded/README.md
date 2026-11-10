# Superseded runs

Earlier, partial leaderboard runs, kept as raw data. No document is generated from them: each was
measured with an older binary or an older version of the harness, as its own `environment` and
`settings` record.

| File | Benchmarks | Interleaved blocks | scrutate binary (SHA-256) |
|---|---|---|---|
| `leaderboard-warm-prelim.json` | 15 | no | not recorded |
| `leaderboard-warm-blocks.json` | 9 | no | `53a90727e157...` |
| `leaderboard-warm-prevbin.json` | 33 | 3 | `53a90727e157...` |
| `leaderboard-warm-eeab.json` | 35 | 3 | `eeab24663509...` |
| `leaderboard-warm-a82a.json` | 37 | 3 | `a82a23c54270...` |
| `leaderboard-warm-fb5c.json` (and `-cold`, `indexed-`, `compare-`, `chain-`) | 37 | 3 | `fb5c4e0a1250...` |
| `leaderboard-warm-fb3b.json` (and `-cold`, `indexed-`, `compare-`, `chain-`) | 37 | 3 | `fb3bd578c6d9...` |
| `leaderboard-warm-b57d.json` (and `-cold`, `indexed-`, `compare-`, `chain-`) | 37 | 3 | `b57d2a666dba...` |

`leaderboard-warm-prevbin` stopped at `patho/backtracking trap`, where the reference (Python's
backtracking regex engine) never finished. Its binary predates searching a large file on all
threads and suffix-literal candidate lines. `leaderboard-warm-eeab` has both but predates the single-thread
walk for small trees and the one-buffer read of files without line breaks; it stopped before the two
structural benchmarks, which the harness then called without `--json`.

docs/LEADERBOARD.md is generated from `../leaderboard-warm.json`, `../leaderboard-cold.json` and
`../indexed.json`.

`indexed-eeab.json` and `compare-eeab.json` were measured with the same `eeab24663509...` binary (the latter before
the harness measured peak memory on macOS).
`leaderboard-warm-a82a.json` is the last full run before regexes were built with `^` and `$` as line
anchors (without that, patterns using them were matched one line at a time) and before the structural
benchmarks compared the same query in both tools.

The `fb5c` files are a complete run of the binary before the parallel walker's idle wait was
shortened (vendored `ignore`); the current files repeat every step with the release binary.

`*-fb3b` is the run of the first 0.1.0 build. Its `linux` and `linux-nodot` corpora shared files
through hard links, which doubles the cost of opening a file on macOS for every tool, and its
binary predates the changes listed under "Performance" in CHANGELOG.md.

`*-b57d` is a complete run (37 of 37 warm, 5 of 5 cold and every indexed metric PASS) of the build
before the Linux read-path changes, the operation module and the other refactors of the same day;
the published run is of the binary that includes them.

`leaderboard-warm-587a.json` is a complete warm run (36 of 37 PASS) of the build before output was
held for a millisecond and a large one-line file was read on several threads. Its one PARTIAL is
`patho/backtracking trap`: 17.0 ms against ugrep's 17.0 ms, where the b57d run had 15.8 against
16.2. `leaderboard-cold-587a-partial.json` holds the two rows measured before that chain was
stopped to work on the tie.
