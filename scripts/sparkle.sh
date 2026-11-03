#!/usr/bin/env bash
# Fetches the pinned Sparkle release (the updater framework and its signing tools) into .cache/
# once, verified by SHA-256, and prints the directory it unpacked to. It stays out of target/,
# which CI's build cache prunes.
set -euo pipefail

cd "$(dirname "$0")/.."
version="2.10.1"
sha256="1a856d2a9edb8e628de88121d7be04cf0e2167f8641650b78fca00b14244cf27"
dir=".cache/sparkle-$version"

# The marker goes in last, so an interrupted unpack is fetched again.
if [[ ! -f "$dir/.unpacked" ]]; then
  archive="$(mktemp -d)/Sparkle-$version.tar.xz"
  curl -fsSL "https://github.com/sparkle-project/Sparkle/releases/download/$version/Sparkle-$version.tar.xz" -o "$archive"
  echo "$sha256  $archive" | shasum -a 256 -c - >/dev/null
  rm -rf "$dir"
  mkdir -p "$dir"
  tar -xJf "$archive" -C "$dir"
  touch "$dir/.unpacked"
fi
echo "$dir"
