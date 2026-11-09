#!/usr/bin/env bash
# Build the release artifacts for the current platform without CI: binary tarball, wheel, SBOM and
# checksums. The release workflow does the same for every platform and adds Sigstore attestations.
set -euo pipefail
cd "$(dirname "$0")/.."
v=$(grep -m1 '^version = ' crates/scrutate/Cargo.toml | cut -d'"' -f2)
target=$(rustc -vV | sed -n 's/^host: //p')
rm -rf dist && mkdir -p dist
cargo build --release -p scrutate --locked
name="scrutate-$v-$target"
rm -rf "dist/$name" && mkdir -p "dist/$name"
cp target/release/scrutate LICENSE README.md CHANGELOG.md "dist/$name/"
cp -r docs "dist/$name/docs"
cp -r integrations "dist/$name/integrations"
tar -C dist -czf "dist/$name.tar.gz" "$name"
rm -rf "dist/$name"
if command -v maturin >/dev/null; then
  maturin build --release --strip -m crates/scrutate-py/Cargo.toml -o dist
fi
python3 tools/sbom.py --out "dist/scrutate-$v.cdx.json"
(cd dist && sha256sum * > SHA256SUMS)
echo "artifacts:"; ls -la dist
