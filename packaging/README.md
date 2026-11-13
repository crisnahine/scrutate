# Packaging

Package definitions for Scrutate 0.1.0. None is published: no release is tagged yet, so the
release URLs below do not exist. Every test's log is in `bench/results/platforms/packages/`.

| Path | What it is |
|---|---|
| `homebrew/scrutate.rb` | Homebrew formula. Builds from the source tarball of tag `v0.1.0` with the Homebrew `rust`, installs bash, zsh and fish completions. |
| `homebrew/tap/` | The files of the tap repository `scrutate/homebrew-tap` as `brew tap-new --github-packages scrutate/tap` writes them with Homebrew 7.0.13 (its `tap-new-*.yml` templates; tap-new picks the autobump hour at random): `tests.yml` (`brew test-bot` on pushes and pull requests, bottles on pull requests), `publish.yml` (`brew pr-pull` by dispatch, bottles to GitHub Packages, attestations), `autobump.yml` (a daily `brew bump`), `dependabot.yml`, and a README with `brew trust --tap`. |
| `scoop/scrutate.json` | Scoop manifest for the Windows x86_64 and ARM64 release zips, with `checkver` and `autoupdate` (hashes from `SHA256SUMS`). |
| `scoop/bucket/` | The files of the bucket repository `scrutate/scoop-bucket`, from `ScoopInstaller/BucketTemplate` (commit `dd0ce8a`, 2026-08-24): Excavator (`excavator.yml`, every 4 hours, updates the manifest from `checkver` and `autoupdate`), the bucket tests (`ci.yml`), the `bin/` scripts with `auto-pr.ps1` pointed at `scrutate/scoop-bucket:main`, and a README. Left out: the issue and pull request handlers and the template's CRLF `.gitattributes`. |
| `winget/manifests/c/crisnahine/Scrutate/0.1.0/` | winget manifests (version, installer, default locale; schema 1.12.0). Portable install from the same zips. |
| `linux/build-packages.sh` | Builds the `.deb` (cargo-deb) and `.rpm` (cargo-generate-rpm) in a `rust:1-slim-bookworm` container. The package metadata is in `crates/scrutate/Cargo.toml` under `[package.metadata.deb]` and `[package.metadata.generate-rpm]`. |
| `linux/test-packages.sh` | Installs a package in a clean container, runs `scrutate --version` and a `scrutate grep`, removes it. |
| `validate_manifests.py` | Parses the Scoop and winget files, checks them against the published schemas and fails on an all-zero sha256 in any manifest. Until a release exists the hashes are placeholders, so use `--draft`: `uvx --with jsonschema --with pyyaml python packaging/validate_manifests.py --draft`. |

## How each was tested

All on 2026-11-09, Apple Silicon host, Docker through colima.

**Homebrew.** Installed from a `git archive --prefix=scrutate-0.1.0/` tarball of the source,
through a throwaway tap with the `url` swapped for `file://` and the real `sha256`.

- macOS (`homebrew-macos.log`): not possible here. `brew install --build-from-source` stops before
  building: Xcode 26.6 is installed and Homebrew requires Xcode 27 on macOS 27. Nothing was
  installed or upgraded; the downloaded bottles were deleted and the tap removed.
- Linux (`homebrew-linux.log`): `homebrew/brew` arm64 container after `brew update`, Homebrew
  7.0.13. `brew install --build-from-source` built it with `cargo install` in 2 min 53 s,
  `scrutate --version` printed `scrutate 0.1.0`, the bash, zsh and fish completions were installed,
  `brew test` passed (version and a `scrutate grep`), `brew uninstall` removed it.

The formula logic is the same on both; the macOS run still needs a machine with a current Xcode
(or with Xcode removed and only the Command Line Tools).

**Scoop and winget** (`manifests-validate.log`): all four files parse and pass the published
schema (Scoop `schema.json`, winget 1.12.0). Not installed: the release zips do not exist yet and
no Windows machine was available.

**Debian and RPM.** `linux/build-packages.sh` in `rust:1-slim-bookworm` (log `build.log`) built
`scrutate_0.1.0-1_arm64.deb` (18.8 MB, `Depends: libc6 (>= 2.34)`) and
`scrutate-0.1.0-1.aarch64.rpm` (20.2 MB) into `dist/`. `linux/test-packages.sh` then passed in a
clean `debian:bookworm-slim` (`test-deb-debian-bookworm.log`) and a clean `fedora` 45
(`test-rpm-fedora.log`): install, `scrutate --version`, `scrutate grep` with the exact expected line,
uninstall, binary gone. The RPM is unsigned and Fedora 45 refuses unsigned packages by default, so
the test uses `rpm -i --nosignature`. x86_64 packages were not built.

**crates.io** (`cargo-package.log`): `cargo package -p scrutate --list` lists 112 files and the
`.crate` is 164 KB, but the verify build fails: `src/models.rs` embeds `models/semantic.npz` and
`models/ranker.npz` from the workspace root with `include_bytes!`, outside the package. Moving them
into the crate is not enough: `semantic.npz` is 14.6 MB, still 13.3 MB after gzip, over the 10 MB
crates.io limit. Options: shrink the model (f16 or int8 weights, say) or download it on first
use. Second blocker: the build needs the workspace's
`[patch.crates-io] ignore = { path = "vendor/ignore" }`, and Cargo reads `[patch]` only from the
workspace root, so a crate from crates.io would build against upstream `ignore`, which lacks the
changes `instant.rs` calls (`grow_threads`). The fork would first have to be published under its
own name. Nothing was published.

## At release time

1. Tag `v0.1.0` and let `.github/workflows/release.yml` publish the archives and `SHA256SUMS`.
2. Homebrew: create the public repository `scrutate/homebrew-tap` and commit the files of
   `homebrew/tap/` to its `main`, with `homebrew/scrutate.rb` as `Formula/scrutate.rb` and its
   `sha256` set to `shasum -a 256` of
   `https://github.com/scrutate/scrutate/archive/refs/tags/v0.1.0.tar.gz`, after
   `brew audit --strict --online scrutate/tap/scrutate`. Users run `brew trust --tap
   scrutate/tap` first. `autobump.yml` opens a pull request for each new tag; a pull request
   opened with `GITHUB_TOKEN` starts no workflow, so its tests run only when re-run by hand or
   when the tap gets a GitHub App token. homebrew-core takes no repository younger than 30 days,
   nor, when its own author submits it, one below 90 forks, 90 watchers or 225 stars.
3. Scoop: create the public repository `scrutate/scoop-bucket` (from `ScoopInstaller/BucketTemplate`,
   or empty), commit the files of `scoop/bucket/` and `scoop/scrutate.json` as
   `bucket/scrutate.json` with both `hash` values from `SHA256SUMS`, and in Settings, Actions,
   General allow all actions and keep "Read repository contents and packages permissions"
   (Excavator's workflow asks for `contents: write` itself). Later versions update through
   Excavator and `autoupdate`. ScoopInstaller/Extras wants an issue before a new manifest; Main
   needs 500 stars and 150 forks.
4. winget: set both `InstallerSha256` values from `SHA256SUMS`, run
   `winget validate --manifest packaging/winget/manifests/c/crisnahine/Scrutate/0.1.0` on Windows, then
   open a PR to microsoft/winget-pkgs (`wingetcreate submit` does both).
5. Debian and RPM: run `linux/build-packages.sh` on each target (x86_64 and aarch64) and attach the
   packages, signed (`rpm --addsign`) so Fedora accepts the RPM, to the release. Add this to
   `release.yml` so every tag does it.
6. crates.io: not possible (see above) until the models are smaller or fetched separately and the
   `ignore` fork is published.

The zero hashes in the Homebrew, Scoop and winget files are placeholders that fail any install
until replaced. Then `validate_manifests.py` without `--draft` must pass.
