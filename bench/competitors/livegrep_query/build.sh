#!/usr/bin/env bash
# Build lgq against livegrep's protocol definition at the pinned revision.
#   docker run --rm -v "$PWD/bench/competitors/livegrep_query":/src:ro -v "$HOME/.cache/scrutate-bench/livegrep":/out golang:1.26-bookworm bash /src/build.sh
set -euo pipefail
REV=${LIVEGREP_REV:-923d5ad71dfe60900e6c2017b2fa4a5ff902ad71}
apt-get update -qq && apt-get install -y -qq protobuf-compiler > /dev/null
export PATH=$PATH:$(go env GOPATH)/bin
go install google.golang.org/protobuf/cmd/protoc-gen-go@v1.36.10
go install google.golang.org/grpc/cmd/protoc-gen-go-grpc@v1.5.1
mkdir -p /build/src/proto /build/pb && cd /build
for f in livegrep.proto config.proto; do
  curl -fsSL -o src/proto/$f "https://raw.githubusercontent.com/livegrep/livegrep/$REV/src/proto/$f"
done
M="Msrc/proto/livegrep.proto=lgq/pb,Msrc/proto/config.proto=lgq/pb"
protoc -I . --go_out=pb --go_opt=paths=source_relative,$M --go-grpc_out=pb --go-grpc_opt=paths=source_relative,$M src/proto/livegrep.proto src/proto/config.proto
mv pb/src/proto/*.go pb/ && rm -rf pb/src
sed -i 's/^package .*/package pb/' pb/*.go
cp /src/main.go .
go mod init lgq
go mod tidy
CGO_ENABLED=0 go build -o /out/lgq .
/out/lgq 2>&1 | head -1 || true
echo built
