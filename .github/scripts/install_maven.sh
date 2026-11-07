#!/usr/bin/env bash
# Installs the pinned Maven into $1 (default $RUNNER_TEMP/maven). CI and release builds both use
# it, so they build with the same Maven. Maven Central first (archive.apache.org drops
# connections); the checksum pins the same file from either source.
set -euo pipefail
version=3.10.0
sha256=a46cc51bc74fa23fd267c7a0b9132b146dcf526da60d31aa5174e565632e9e0e
dest="${1:-${RUNNER_TEMP:?}/maven}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
archive="$tmp/maven.tar.gz"
for url in \
  "https://repo.maven.apache.org/maven2/org/apache/maven/apache-maven/$version/apache-maven-$version-bin.tar.gz" \
  "https://archive.apache.org/dist/maven/maven-3/$version/binaries/apache-maven-$version-bin.tar.gz"; do
  curl --fail --silent --show-error --location --connect-timeout 20 \
    --retry 3 --retry-all-errors "$url" -o "$archive" && break
done
echo "$sha256  $archive" | sha256sum --check -
mkdir -p "$dest"
# cd first: GNU tar on Windows reads `D:` in `-C D:\...` as a remote host.
(cd "$dest" && tar -xzf "$archive" --strip-components=1)
"$dest/bin/mvn" -v | head -1
