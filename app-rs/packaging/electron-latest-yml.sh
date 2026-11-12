#!/usr/bin/env bash
# The bridge from the Electron app (#166 phase 6): electron-updater in every Electron install
# reads latest-linux.yml / latest.yml / latest-mac.yml from the newest release. Publishing them
# for the GPUI build, pointing at its packages, makes those installs download, verify (sha512)
# and install the GPUI app themselves; same package name, so it replaces them in place.
# Usage: electron-latest-yml.sh <dist dir> <version> <out.yml> <primary file> [more files…]
set -euo pipefail
dist=$1 version=$2 out=$3
shift 3
sha512() { sha512sum "$dist/$1" | cut -d' ' -f1 | xxd -r -p | base64 -w0; }
size() { stat -c %s "$dist/$1"; }
{
  echo "version: $version"
  echo "files:"
  for f in "$@"; do
    echo "  - url: $f"
    echo "    sha512: $(sha512 "$f")"
    echo "    size: $(size "$f")"
  done
  echo "path: $1"
  echo "sha512: $(sha512 "$1")"
  echo "releaseDate: '$(date -u +%Y-%m-%dT%H:%M:%S.000Z)'"
} >"$dist/$out"
echo "wrote $dist/$out"
