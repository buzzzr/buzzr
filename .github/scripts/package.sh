#!/usr/bin/env bash
# Package a built buzzr binary as a release archive.
#   package.sh <binary> <target-triple> <version> <out-dir>
# Writes <out-dir>/buzzr-<version>-<target>.tar.gz holding buzzr/{buzzr,LICENSE,README.md}.
set -euo pipefail
bin="$1" target="$2" version="$3" out="$4"
name="buzzr-${version}-${target}"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name" "$out"
cp "$bin" "$stage/$name/buzzr"
chmod 755 "$stage/$name/buzzr"
cp LICENSE README.md "$stage/$name/"
tar -C "$stage" -czf "$out/$name.tar.gz" "$name"
echo "$out/$name.tar.gz"
