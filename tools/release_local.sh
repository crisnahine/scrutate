#!/usr/bin/env bash
# Build the release artifacts for the current platform without CI: binary tarball, wheel, SBOM and
# checksums. The release workflow does the same for every platform and adds Sigstore attestations.
set -euo pipefail
cd "$(dirname "$0")/.."
v=$(grep -m1 '^version = ' crates/scrutate/Cargo.toml | cut -d'"' -f2)
target=$(rustc -vV | sed -n 's/^host: //p')
rm -rf dist && mkdir -p dist
cargo build --release -p scrutate --locked
# the binary must need only the operating system's own libraries
if command -v otool > /dev/null; then
  if otool -L target/release/scrutate | tail -n +2 | grep -v -E '^[[:space:]]+(/usr/lib/|/System/)'; then
    echo "target/release/scrutate links a library outside the system"; exit 1
  fi
elif command -v ldd > /dev/null; then
  if ldd target/release/scrutate | grep -v -E 'linux-vdso|ld-linux|lib(c|m|dl|pthread|rt|gcc_s)\.so'; then
    echo "target/release/scrutate links a library outside the system"; exit 1
  fi
fi
name="scrutate-$v-$target"
rm -rf "dist/$name" && mkdir -p "dist/$name"
cp target/release/scrutate LICENSE README.md CHANGELOG.md "dist/$name/"
python3 tools/third_party_notices.py --out "dist/$name/THIRD_PARTY_NOTICES.txt"
cp -r docs "dist/$name/docs"
cp -r integrations "dist/$name/integrations"
tar -C dist -czf "dist/$name.tar.gz" "$name"
rm -rf "dist/$name"
if command -v maturin >/dev/null; then
  cmp -s LICENSE crates/scrutate-py/LICENSE || { echo "crates/scrutate-py/LICENSE (the wheel's copy) differs from LICENSE"; exit 1; }
  python3 tools/third_party_notices.py --package scrutate-py --out crates/scrutate-py/THIRD_PARTY_NOTICES.txt
  maturin build --release --strip -m crates/scrutate-py/Cargo.toml -o dist
fi
python3 tools/sbom.py --out "dist/scrutate-$v.cdx.json"
(cd dist && sha256sum * > SHA256SUMS)
echo "artifacts:"; ls -la dist
