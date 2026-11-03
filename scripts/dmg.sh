#!/usr/bin/env bash
# Wraps Annuncia.app in the disk image the website offers: a window with the app, an arrow and
# the Applications folder to drag it to. Signs it when ANNUNCIA_SIGN_IDENTITY is set; release.sh
# notarizes it. Sparkle updates use the zip instead.
#
#   scripts/dmg.sh target/release/Annuncia.app target/release/Annuncia.dmg
set -euo pipefail

cd "$(dirname "$0")/.."
app="$1"
out="$2"
version="1.6.7"

# dmgbuild writes the window's layout itself, with no Finder scripting, so it works headless.
if command -v uvx >/dev/null; then
  dmgbuild=(uvx --quiet --from "dmgbuild==$version" dmgbuild)
else
  venv=".cache/dmgbuild-$version"
  if [[ ! -x "$venv/bin/dmgbuild" ]]; then
    python3 -m venv "$venv"
    "$venv/bin/pip" install --quiet "dmgbuild==$version"
  fi
  dmgbuild=("$venv/bin/dmgbuild")
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# One background with both resolutions, so the dither stays sharp on Retina screens.
tiffutil -cathidpicheck assets/dmg/background.png assets/dmg/background@2x.png \
  -out "$work/background.tiff" 2>/dev/null

rm -f "$out"
"${dmgbuild[@]}" -s scripts/dmg-settings.py \
  -D app="$app" \
  -D background="$work/background.tiff" \
  -D icon="$app/Contents/Resources/AppIcon.icns" \
  Annuncia "$out"
if [[ -n "${ANNUNCIA_SIGN_IDENTITY:-}" ]]; then
  codesign --force --sign "$ANNUNCIA_SIGN_IDENTITY" --timestamp "$out"
fi
echo "$out"
