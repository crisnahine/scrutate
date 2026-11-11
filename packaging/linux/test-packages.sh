#!/bin/sh
# Install-test a package in a clean container, then remove it. Run from the repository root:
#   docker run --rm -v "$PWD/dist":/dist:ro -v "$PWD/packaging/linux":/pkg:ro debian:bookworm-slim sh /pkg/test-packages.sh deb
#   docker run --rm -v "$PWD/dist":/dist:ro -v "$PWD/packaging/linux":/pkg:ro fedora sh /pkg/test-packages.sh rpm
set -eux
case "$1" in
    deb) apt-get install -y /dist/scrutate_*.deb ;;
    rpm) rpm -i --nosignature /dist/scrutate-*.rpm ;;  # unsigned until the release signs it
esac
command -v scrutate
scrutate --version
mkdir -p /tmp/t && printf 'def greet():\n    return "hello"\n' > /tmp/t/hello.py
out=$(scrutate grep --no-color greet /tmp/t)
echo "$out"
[ "$out" = "/tmp/t/hello.py:1:def greet():" ]
case "$1" in
    deb) dpkg -s scrutate | head -12; apt-get remove -y scrutate ;;
    rpm) rpm -qi scrutate; rpm -e scrutate ;;
esac
test ! -e /usr/bin/scrutate
echo "PASS: $1 installed, ran, removed"
