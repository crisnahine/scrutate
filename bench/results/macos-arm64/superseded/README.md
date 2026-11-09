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
