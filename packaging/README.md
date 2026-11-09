# Packaging

Package definitions for Scrutate 0.1.0. None is published: the repository is not public, so the
release URLs below do not exist yet. Logs of every test are in `bench/results/platforms/packages/`.

| Path | What it is |
|---|---|
| `homebrew/scrutate.rb` | Homebrew formula. Builds from the source tarball of tag `v0.1.0` with the Homebrew `rust`, installs bash, zsh and fish completions. |
| `scoop/scrutate.json` | Scoop manifest for the Windows x86_64 and ARM64 release zips, with `checkver` and `autoupdate` (hashes from `SHA256SUMS`). |
| `winget/manifests/c/crisnahine/Scrutate/0.1.0/` | winget manifests (version, installer, default locale; schema 1.9.0). Portable install from the same zips. |
| `linux/build-packages.sh` | Builds the `.deb` (cargo-deb) and `.rpm` (cargo-generate-rpm) in a `rust:1-slim-bookworm` container. The package metadata is in `crates/scrutate/Cargo.toml` under `[package.metadata.deb]` and `[package.metadata.generate-rpm]`. |
| `linux/test-packages.sh` | Installs a package in a clean container, runs `scrutate --version` and a `scrutate grep`, removes it. |
| `validate_manifests.py` | Parses the Scoop and winget files and checks them against the published schemas. |

## How each was tested

All on 2026-11-09, Apple Silicon host, Docker through colima. Logs in
`bench/results/platforms/packages/`.

**Homebrew.** The formula was installed from a `git archive --prefix=scrutate-0.1.0/` tarball of
the source, through a throwaway tap with the `url` swapped for `file://` and the real `sha256`.

- macOS (`homebrew-macos.log`): not possible on this machine. `brew install --build-from-source`
  stops before building because Xcode 26.6 is installed and Homebrew requires Xcode 27 on macOS 27.
  Nothing was installed or upgraded; the downloaded bottles were deleted and the tap removed.
- Linux (`homebrew-linux.log`): in the `homebrew/brew` arm64 container after `brew update`,
  Homebrew 7.0.13. `brew install --build-from-source` built it with `cargo install` in 2 min 53 s,
  `scrutate --version` printed `scrutate 0.1.0`, the bash, zsh and fish completions were installed,
  `brew test` passed (version and a `scrutate grep`), `brew uninstall` removed it.

The formula logic is the same on both systems; the macOS run still needs a machine with a current
Xcode (or with Xcode removed and only the Command Line Tools).

**Scoop and winget** (`manifests-validate.log`): all four files parse, and each passes the published
schema (Scoop `schema.json`, winget 1.9.0). Not installed: the release zips do not exist yet and no
Windows machine was available.

**Debian and RPM.** `linux/build-packages.sh` in `rust:1-slim-bookworm` (log `build.log`) built
`scrutate_0.1.0-1_arm64.deb` (18.8 MB, `Depends: libc6 (>= 2.34)`) and
`scrutate-0.1.0-1.aarch64.rpm` (20.2 MB) into `dist/`. `linux/test-packages.sh` then passed in a
clean `debian:bookworm-slim` (`test-deb-debian-bookworm.log`) and a clean `fedora` 45
(`test-rpm-fedora.log`): install, `scrutate --version`, `scrutate grep` with the exact expected line,
uninstall, binary gone. The RPM is unsigned, and Fedora 45 refuses unsigned packages by default, so
the test installs it with `rpm -i --nosignature`. x86_64 packages were not built.

**crates.io** (`cargo-package.log`): `cargo package -p scrutate --list` lists 112 files and the
`.crate` is 164 KB, but the verify build fails: `src/lib.rs` embeds `models/semantic.npz` and
`models/ranker.npz` from the workspace root with `include_bytes!`, and those files are outside the
package. Moving them into the crate would not be enough: `semantic.npz` is 14.6 MB and still 13.3 MB
after gzip, over the 10 MB crates.io limit. Options: shrink the model (for example f16 or int8
weights), or download it on first use. Nothing was published.

## At release time

1. Tag `v0.1.0` and let `.github/workflows/release.yml` publish the archives and `SHA256SUMS`.
2. Homebrew: set `sha256` in `homebrew/scrutate.rb` to `shasum -a 256` of
   `https://github.com/crisnahine/scrutate/archive/refs/tags/v0.1.0.tar.gz`, then open a PR to a tap
   (`crisnahine/homebrew-tap`) or to homebrew-core. Run `brew audit --strict --online scrutate` first.
3. Scoop: set both `hash` values in `scoop/scrutate.json` from `SHA256SUMS`, then open a PR to a
   bucket (your own, or ScoopInstaller/Extras). Later versions update through `autoupdate`.
4. winget: set both `InstallerSha256` values from `SHA256SUMS`, run
   `winget validate --manifest packaging/winget/manifests/c/crisnahine/Scrutate/0.1.0` on Windows, then
   open a PR to microsoft/winget-pkgs (`wingetcreate submit` does both).
5. Debian and RPM: run `linux/build-packages.sh` on each target (x86_64 and aarch64) and attach the
   packages to the release, signed (`rpm --addsign`) so Fedora accepts the RPM. Add this to
   `release.yml` so it happens on every tag.
6. crates.io: see the note above; not possible until the models are smaller or fetched separately.

The zero hashes in the Homebrew, Scoop and winget files are placeholders and will fail any install
until they are replaced.
