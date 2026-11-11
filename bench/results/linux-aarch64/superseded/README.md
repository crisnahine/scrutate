# Superseded runs

Earlier runs, kept as raw data. No document is generated from them.

| File | scrutate binary (SHA-256) | Virtual CPUs |
|---|---|---|
| `leaderboard-warm-6602.json` (and `-cold`, `indexed-`, `bench-`) | `6602bbe58900...` | 8 |
| `leaderboard-warm-558e.json` (and `-cold`, `indexed-`, `bench-`) | `558e6e6b109f...` | 5 |
| `leaderboard-warm-e49f.json` (and `-cold`, `indexed-`, `bench-`) | `e49f3fbf80a2...` | 5 |
| `leaderboard-warm-261e.json` (and `-cold`, `indexed-`, `bench-`) | `261ed1a2a461...` | 5 |

`*-6602` is the run of the first 0.1.0 build, in a virtual machine with 8 CPUs on a host with 5
performance cores: three virtual CPUs ran on efficiency cores at any time, and the medians of every
tool moved by 10 to 30% between blocks. Its binary predates the changes listed under "Performance"
in CHANGELOG.md.

`*-558e` is the run of the build before a tree read from the disk got more threads than cores on
Linux. Warm cache: 14 of 14 PASS. Indexed: every metric PASS. Cold cache: `linux/literal` and
`linux/complex regex` PASS, `linux/case-insensitive` and `linux/no match` FAIL by 0.2% and 0.7%
against git grep, with every tool within 3% of the time five threads take to read the files. Its
read-files ceiling used five threads that shared a directory, which is slower than the disk allows.

`*-e49f` is a complete run (14 of 14 warm, 4 of 4 cold and every indexed metric PASS) of the build
before output was held for a millisecond and a large one-line file was read on several threads.
Its thinnest row was `linux/very many matches`, 0.950 of ripgrep's time with an interval up to
0.995.

`*-261e` is a complete run (14 of 14 warm, 4 of 4 cold and every indexed metric PASS) of the build
before the MCP server served revision 2026-07-28 and `refs` followed the types of variables and
TypeScript path aliases. Engine A was the same code.
