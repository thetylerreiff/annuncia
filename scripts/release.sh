#!/usr/bin/env bash
# Publishes the version in Cargo.toml: builds and notarizes Annuncia.app, uploads it to a GitHub
# release, and adds it to the update feed (site/public/appcast.xml), which goes live when the
# site deploys. Run from a clean main after bumping the version.
#
# One-time setup:
#   xcrun notarytool store-credentials annuncia --apple-id <you> --team-id <team>
# The Sparkle signing key lives in the login keychain (account "annuncia", from generate_keys).
set -euo pipefail

cd "$(dirname "$0")/.."
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
tag="v$version"
repo="thetylerreiff/annuncia"
feed="site/public/appcast.xml"

if [[ -n "$(git status --porcelain)" ]]; then
  echo "error: commit or stash your changes first" >&2
  exit 1
fi
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  echo "error: $tag already exists; bump the version in Cargo.toml" >&2
  exit 1
fi
identity="${ANNUNCIA_SIGN_IDENTITY:-$(security find-identity -v -p codesigning \
  | grep -E '"Developer ID Application:' | head -n1 | awk '{print $2}')}"
if [[ -z "$identity" ]]; then
  echo "error: releases need a Developer ID Application certificate" >&2
  exit 1
fi

app="$(ANNUNCIA_SIGN_IDENTITY="$identity" ANNUNCIA_TIMESTAMP=1 scripts/bundle.sh release | tail -n1)"
out="target/release/dist"
rm -rf "$out"
mkdir -p "$out"
zip="$out/Annuncia.zip"

# Notarize, then staple the ticket so Gatekeeper accepts the app offline, and zip it again.
ditto -c -k --keepParent "$app" "$zip"
xcrun notarytool submit "$zip" --keychain-profile annuncia --wait
xcrun stapler staple "$app"
rm "$zip"
ditto -c -k --keepParent "$app" "$zip"

# The feed entry: Sparkle checks the EdDSA signature against SUPublicEDKey before installing.
signature="$("$(scripts/sparkle.sh)/bin/sign_update" --account annuncia "$zip")"
item="    <item>
      <title>Version $version</title>
      <pubDate>$(LC_ALL=C date -u "+%a, %d %b %Y %H:%M:%S +0000")</pubDate>
      <sparkle:version>$version</sparkle:version>
      <sparkle:shortVersionString>$version</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>15.0</sparkle:minimumSystemVersion>
      <enclosure url=\"https://github.com/$repo/releases/download/$tag/Annuncia.zip\" type=\"application/octet-stream\" $signature/>
    </item>"
# Newest first, right after the marker.
ITEM="$item" perl -0pi -e 's/(<!-- releases -->\n)/$1$ENV{ITEM}\n/' "$feed"

# The download has to exist before the feed points at it.
git tag "$tag"
git push origin "$tag"
gh release create "$tag" "$zip" --repo "$repo" --title "Annuncia $version" --generate-notes
git add "$feed"
git commit -m "Release $version"
git push origin HEAD
echo "Released $version"
