# Superseded runs

Earlier runs, kept as raw data. No document is generated from them.

| File | scrutate binary (SHA-256) | Virtual CPUs |
|---|---|---|
| `leaderboard-warm-6602.json` (and `-cold`, `indexed-`, `bench-`) | `6602bbe58900...` | 8 |
| `leaderboard-warm-558e.json` (and `-cold`, `indexed-`, `bench-`) | `558e6e6b109f...` | 5 |

`*-6602` is the run of the first 0.1.0 build, in a virtual machine with 8 CPUs on a host with 5
performance cores: three virtual CPUs ran on efficiency cores at any time, and the medians of every
tool moved by 10 to 30% between blocks. Its binary predates the changes listed under "Performance"
in CHANGELOG.md.

`*-558e` is the run of the build before a tree read from the disk got more threads than cores on
Linux. Warm cache: 14 of 14 PASS. Indexed: every metric PASS. Cold cache: `linux/literal` and
`linux/complex regex` PASS, `linux/case-insensitive` and `linux/no match` FAIL by 0.2% and 0.7%
against git grep, with every tool within 3% of the time five threads take to read the files. Its
read-files ceiling used five threads that shared a directory, which is slower than the disk allows.
