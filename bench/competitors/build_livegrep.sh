#!/usr/bin/env bash
# Build livegrep's codesearch backend, its web frontend and the `lg` command-line client the way its README says
# (bazel via bazelisk, the version pinned in .bazelversion), on Ubuntu 24.04 like its CI.
#   docker run --rm -v "$PWD/bench/competitors":/src:ro -v "$PWD/bench/competitors/out":/out ubuntu:24.04 bash /src/build_livegrep.sh
set -euo pipefail
REV=${LIVEGREP_REV:-923d5ad71d}
apt-get update -qq
apt-get install -y -qq git curl ca-certificates build-essential python3 zip unzip openjdk-21-jdk-headless > /dev/null
# bazelisk 1.25.0 publishes no checksum: these are the hashes of the two files as downloaded on 2026-11-12
case $(uname -m) in
  aarch64) barch=arm64; sum=4c8d966e40ac2c4efcc7f1a5a5cceef2c0a2f16b957e791fa7a867cce31e8fcb ;;
  x86_64) barch=amd64; sum=fd8fdff418a1758887520fa42da7e6ae39aefc788cf5e7f7bb8db6934d279fc4 ;;
esac
curl -fsSL -o /usr/local/bin/bazel "https://github.com/bazelbuild/bazelisk/releases/download/v1.25.0/bazelisk-linux-$barch"
echo "$sum  /usr/local/bin/bazel" | sha256sum -c - || { rm -f /usr/local/bin/bazel; exit 1; }
chmod +x /usr/local/bin/bazel
git clone -q https://github.com/livegrep/livegrep.git /livegrep
cd /livegrep
git checkout -q "$REV"
bazel build -c opt //src/tools:codesearch //cmd/lg //cmd/livegrep
mkdir -p /out
cp -L bazel-bin/src/tools/codesearch /out/
cp -L "$(find -L bazel-bin/cmd/lg -type f -name lg -perm -u+x | head -1)" /out/lg
cp -L "$(find -L bazel-bin/cmd/livegrep -type f -name livegrep -perm -u+x | head -1)" /out/livegrep
rm -rf /out/web && cp -rL "$(dirname "$(find -L bazel-bin/cmd/livegrep -name hashes.txt -path "*runfiles*" | head -1)")" /out/web
git rev-parse HEAD > /out/REVISION
/out/codesearch --help 2>&1 | head -3 || true
echo built
