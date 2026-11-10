# Security assessment (0.1.0)

Scope: the `scrutate` binary (Engine A, Engine B, CLI, MCP server) and the Python bindings, as
built from this repository on 2026-11-10. This is a self-assessment by the authors with the tests
named below; it is **not** an independent audit or penetration test.

## Threat model

Scrutate reads source trees chosen by the user and writes one cache directory (`.scrutate/`) inside
each indexed repository. The assets are (1) the confidentiality of the source code, (2) the
integrity of the machine running the tool, (3) availability (no hangs or resource exhaustion on
hostile inputs). Attackers considered: a **hostile repository** (crafted files, names, symlinks,
`.gitignore`, a corrupted or planted `.scrutate/` directory), a **hostile query** (patterns), and
a **hostile MCP client** (malformed JSON-RPC, path escapes).

## Controls and evidence

| Area | Control | Evidence |
|---|---|---|
| No code execution from repositories | Files are parsed as data (tree-sitter, regex); nothing is evaluated, imported, or run. No build systems, hooks, `.scrutate.toml` scripts or plugins exist. | Code review: no `std::process::Command` outside `cli.rs` tests of other tools; `git` is only invoked by the *Python reference* package's file lister (`rg --files`), never by the Rust engine. |
| No network | The binary makes no network connections and has no HTTP/DNS dependency. | `cargo tree` (docs/DEPENDENCIES.md) contains no networking crate; offline build and run verified in a proxy-restricted environment. |
| Filesystem confinement | `read_code` / `read()` reject paths that resolve outside the repository root; the index is written only under `<root>/.scrutate/`; segment files are written to a temp directory *inside* `.scrutate/seg/` and renamed into place (no `/tmp`, no predictable world-writable paths). | `tests/robustness.rs::symlinks_ignores_and_nested_repos` (symlink escape not followed by default), bindings test `r.read("../etc/passwd")` → error. |
| Symlinks | Not followed by default (Engine A and B); `-L` follows with loop detection (the `ignore` walker). | robustness: loop + dangling + escaping symlinks. |
| Permission-aware | Unreadable files are counted as errors (`--stats`) and skipped, never fatal. | robustness `unreadable_files_are_reported_not_fatal` (skipped when running as root, as in CI containers; the code path is exercised by the error counter). |
| Resource exhaustion: regex | Rust `regex` (linear time, no backtracking) with compiled-size limits (20 MB regex, 10 MB DFA); oversized patterns are rejected with an error. | robustness `regex_limits_and_errors_are_clean`: `(a*)*b` on a 10 000-char line finishes; `(a{1000}){1000}` is an error. |
| Resource exhaustion: files | Engine B skips files > 1 MB and binary files (NUL in the first 4 KB); chunk text is capped; tree-sitter parse per file; Engine A streams with bounded batches; `--max-filesize`, `--max-depth`, `--max-results`, `--max-count`. | robustness `hostile_text_shapes_never_panic` (5 MB line, 200k lines, 20 000-deep nesting). |
| Hostile text | Invalid UTF-8 (lossy decode), BOM, CRLF/CR, UTF-16 with a byte-order mark (transcoded by both engines), multibyte boundaries, emoji identifiers, 255-byte names, spaces/unicode in paths. A real UTF-8 boundary panic was found by this suite and fixed (`text::prefix_bytes`). | same test; `parser_fuzz_smoke` (400 random inputs); proptest `parser_yields_well_formed_chunks`. |
| Index corruption | `PRAGMA quick_check` on open; every segment array validated for dtype/shape/length on open; `scrutate check` runs `integrity_check` + deep validation (sorted ids, posting ranges, IVF permutation) and optional file re-hashing; damaged indexes are rebuilt automatically; a planted non-database file is replaced; locked ≠ corrupt (never deletes a live database). | robustness `corrupted_index_is_detected_and_rebuilt` (5 corruption scenarios); `concurrent_indexers_and_readers_do_not_corrupt` (3 writers + 2 readers, 30× repeated, `integrity_check` ok). |
| Panic containment | A panic in the CLI prints one diagnostic line and exits 2; a panic inside an MCP tool call is reported as a tool error and the server keeps running (poisoned lock recovered). | `cli::main_exit`, `mcp::serve`. |
| Supply chain | 207 crates in the dependency graph of the binary (every platform's), each with a permissive licence (MIT, Apache-2.0, BSD, ISC, Zlib, Unlicense, 0BSD, Unicode-3.0, CC0-1.0); `Cargo.lock` committed; `cargo audit` over the 252 locked crates: **0 vulnerabilities, 0 warnings** (RustSec advisory DB, 2026-11-10). The bundled SQLite was upgraded for RUSTSEC-2026-0459 (`rusqlite` 0.41, `libsqlite3-sys` 0.39). `ignore` is vendored with two documented changes (`vendor/ignore`; the SBOM marks it as a modified copy). `zip` 9, used only by `grep -z`, has no open advisory; `pyo3` was upgraded to 0.30 (2 advisories in 0.27). SBOM generated per release (`tools/sbom.py`, CycloneDX 1.5). | `cargo audit` output in docs/TEST_RESULTS.md; SBOM in release artifacts. |
| Reproducible builds | Release profile pins codegen; `Cargo.lock` and a pinned toolchain (`rust-toolchain.toml`: 1.99.1). Two clean builds in two different build images (`rust:1-slim-bookworm` and `golang:1.26-bookworm` with rustup, both using the pinned 1.99.1 compiler, fresh containers) produced the same SHA-256 for the release commit (`6602bbe5...`, linux/arm64). Reproducibility on other operating systems or across different source paths is not claimed. | `tools/repro_docker.sh`, `bench/results/platforms/repro/`; `tools/reproducible_check.sh` (same machine). |
| Signed releases | Release workflow attaches SHA-256 checksums and Sigstore build-provenance attestations (`actions/attest-build-provenance`) to every binary and wheel. | `.github/workflows/release.yml` — **not yet executed**; no release has been signed at the time of writing. |
| Coverage-guided fuzzing | Four libFuzzer targets (`fuzz/`, nightly toolchain, AddressSanitizer): the `.npy` reader on arbitrary bytes; the text-index soundness property (a text a pattern matches must satisfy the pattern's trigram condition); the edit-distance matcher; every parser tier on arbitrary text under 60 file extensions. It found four real defects, each fixed with a regression test and its crash input kept in `fuzz/regressions/`: chunk line numbers shifted after form feeds (Python-style line splitting); a truncated string array panicked the `.npy` reader; a shape whose element count overflowed wrapped in release builds and could have produced an over-long slice view (undefined behaviour, now a saturating count and checked sizes); a zero-width string array with a huge shape requested a multi-terabyte allocation. | `bench/results/fuzz/` (run logs), `fuzz/regressions/`, unit tests `npy::header_tests::damaged_headers_are_errors_not_panics`, `parse::tests::form_feeds_and_lone_cr_do_not_shift_line_numbers`. |
| Network file systems | A repository on an SMB share (Samba 4.23 server, macOS `smbfs` client): Engine A output identical to the local copy; Engine B indexes, answers, survives three concurrent `index` processes with readers, and passes `check --files`. Found and fixed: a blocking file lock fails at once on SMB (EAGAIN), so the text index polls `try_lock`. One host only: several machines sharing one index over a network file system is not supported (SQLite's documentation warns that its locking is unreliable there). | `tools/netfs_check.sh`, `bench/results/platforms/netfs-smb-macos.txt`. |
| Untrusted `.scrutate.toml` | The configuration file sets option defaults only (booleans, numbers, globs); it cannot name programs or paths to execute. Unknown keys are rejected. | `config.rs` tests. |
| MCP server | Only reads the repository and writes `.scrutate/`; path arguments are confined; malformed requests get JSON-RPC errors; the server is stdio-only (no listening socket). | `mcp.rs`; manual protocol smoke test in docs/TEST_RESULTS.md. |
| Python bindings | Engine behind a `Mutex`; `GrepIter` borrow-checked by PyO3; free-threaded declaration (`gil_used = false`) made after this review; stable ABI build. | `python/tests/test_bindings.py`. |

## Known gaps and residual risks

- **Not independently audited.** No external review or penetration test has been done. The fuzzing
  campaign (above) ran each target for ten minutes on one machine; that finds shallow defects, not
  all of them.
- **`unsafe` inventory** (every site in the Rust crates): `npy.rs` — memory-mapped array views
  (`Mmap::map`; `from_raw_parts` after dtype, length and alignment checks; the `Pod` trait documents
  the invariants; a byte view of a `Pod` slice when saving); `lex.rs` — one `matrixmultiply::sgemm`
  call with lengths derived from the slices it is given; `text.rs` — `from_utf8_unchecked` on a run
  of ASCII word bytes (ASCII boundaries are UTF-8 boundaries); `config.rs` — `std::env::set_var`
  during startup before any thread exists; `cli.rs` — `signal(SIGINT, ...)` (Unix) and
  `SetConsoleCtrlHandler` (Windows) declarations, whose handler only stores to an atomic flag;
  `instant.rs` — one `sysctlbyname("hw.perflevel0.physicalcpu")` call on macOS, writing an `i32`
  whose size it passes;
  `textindex.rs` — `Mmap::map` of segment and large source files (bounds-checked slicing only). tree-sitter grammars and SQLite are C code built from
  crates.io sources.
- **Memory-mapped files can change underneath a reader** if another process rewrites a segment in
  place. Segments are never modified in place (temp dir + rename; deletion only after the manifest
  no longer references them), so this requires an external actor; on Windows, a mapped file cannot
  be deleted and GC retries later.
- **Index privacy**: `.scrutate/` contains a compressed copy of every indexed chunk's text. It is
  ignored by git (`.scrutate/.gitignore` is written automatically) but anyone with read access to
  the repository directory can read it — the same access they already have to the sources.
- **Denial of service by index size**: a repository with millions of files will build a
  multi-gigabyte index; there is no quota. Engine A has no such state.
- **Timing side channels, multi-tenant hosting, encryption at rest**: out of scope for a local
  developer tool.

## Reporting

Please report vulnerabilities privately to the maintainers (see README) rather than in public
issues; include the version (`scrutate --version`), platform and a reproduction.
