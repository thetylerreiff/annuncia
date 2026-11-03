#!/usr/bin/env bash
# Fetches the pinned Sparkle release (the updater framework and its signing tools) into target/
# once, verified by SHA-256, and prints the directory it unpacked to.
set -euo pipefail

cd "$(dirname "$0")/.."
version="2.10.1"
sha256="1a856d2a9edb8e628de88121d7be04cf0e2167f8641650b78fca00b14244cf27"
dir="target/sparkle-$version"

if [[ ! -d "$dir/Sparkle.framework" ]]; then
  archive="$(mktemp -d)/Sparkle-$version.tar.xz"
  curl -fsSL "https://github.com/sparkle-project/Sparkle/releases/download/$version/Sparkle-$version.tar.xz" -o "$archive"
  echo "$sha256  $archive" | shasum -a 256 -c - >/dev/null
  rm -rf "$dir"
  mkdir -p "$dir"
  tar -xJf "$archive" -C "$dir"
fi
echo "$dir"
