#!/usr/bin/env bash
# Packs a CI binary as release.yml packs a release and makes `curl` in later bash steps of the job
# answer that release's two URLs from it, so .github/actions/scrutate installs it before any
# release exists. Every other URL goes to the real curl.
#   fake_release.sh BINARY VERSION      (in a GitHub Actions job; VERSION without the v)
set -euo pipefail
bin=$1 v=$2
case "$RUNNER_OS-$RUNNER_ARCH" in
  Linux-X64) t=x86_64-unknown-linux-musl ;;
  Linux-ARM64) t=aarch64-unknown-linux-musl ;;
  macOS-ARM64) t=aarch64-apple-darwin ;;
  macOS-X64) t=x86_64-apple-darwin ;;
  Windows-X64) t=x86_64-pc-windows-msvc ;;
  Windows-ARM64) t=aarch64-pc-windows-msvc ;;
  *) echo "no release target for $RUNNER_OS-$RUNNER_ARCH"; exit 1 ;;
esac
name=scrutate-$v-$t
temp=$RUNNER_TEMP
# Git Bash: the paths written into the shim and BASH_ENV in its own form
if command -v cygpath > /dev/null; then temp=$(cygpath -u "$temp"); fi
dir=$temp/fake-release
mkdir -p "$dir/$name"
cp "$bin" "$dir/$name/"
chmod +x "$dir/$name/"*
cd "$dir"
if [ "$RUNNER_OS" = Windows ]; then ext=zip; 7z a "$name.zip" "$name" > /dev/null; else ext=tar.gz; tar czf "$name.tar.gz" "$name"; fi
rm -r "$name"
sum() { if command -v sha256sum > /dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
sum -- "$name.$ext" > "$name.sha256"
cat "$name.sha256"

# bash reads BASH_ENV before each run step's script, after Git Bash on Windows has put its own
# curl first on PATH, so a function is the one stand-in that wins on every runner
shim=$temp/fake-release-curl.sh
cat > "$shim" << EOF
curl() {
  local out='' url=\${!#} i
  for ((i = 1; i < \$#; i++)); do [ "\${!i}" = -o ] && { local j=\$((i + 1)); out=\${!j}; }; done
  case \$url in
    https://github.com/scrutate/scrutate/releases/download/v$v/*)
      echo "fake release: \$url from $dir" >&2
      cp "$dir/\${url##*/}" "\$out" ;;
    *) command curl "\$@" ;;
  esac
}
EOF
echo "BASH_ENV=$shim" >> "$GITHUB_ENV"
