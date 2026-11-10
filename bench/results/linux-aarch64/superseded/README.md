# Superseded runs

Earlier runs, kept as raw data. No document is generated from them.

| File | scrutate binary (SHA-256) | Virtual CPUs |
|---|---|---|
| `leaderboard-warm-6602.json` (and `-cold`, `indexed-`, `bench-`) | `6602bbe58900...` | 8 |

`*-6602` is the run of the first 0.1.0 build, in a virtual machine with 8 CPUs on a host with 5
performance cores: three virtual CPUs ran on efficiency cores at any time, and the medians of every
tool moved by 10 to 30% between blocks. Its binary predates the changes listed under "Performance"
in CHANGELOG.md.
