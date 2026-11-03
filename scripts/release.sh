#!/usr/bin/env bash
# Publishes the tagged version in Cargo.toml: builds and notarizes Annuncia.app, uploads it to a
# GitHub release, and adds it to the update feed (site/public/appcast.xml) on main, which goes
# live when the site deploys. The Release workflow runs this when a v* tag is pushed:
#
#   git tag v0.2.0 && git push origin v0.2.0
#
# It also runs locally from the tagged commit. Credentials come from the environment in CI, or
# from the login keychain:
#   Notarization  NOTARY_KEY_PATH, NOTARY_KEY_ID, NOTARY_ISSUER (an App Store Connect API key),
#                 or a profile saved with `xcrun notarytool store-credentials annuncia`
#   Sparkle key   SPARKLE_KEY_FILE (exported with `generate_keys --account annuncia -x`),
#                 or the keychain item generate_keys made
#   GitHub        GH_TOKEN, or a signed-in `gh`
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
if [[ "$(git rev-parse -q --verify "refs/tags/$tag^{commit}" || true)" != "$(git rev-parse HEAD)" ]]; then
  echo "error: HEAD isn't tagged $tag; bump Cargo.toml, then tag and push" >&2
  exit 1
fi
identity="${ANNUNCIA_SIGN_IDENTITY:-$(security find-identity -v -p codesigning \
  | grep -E '"Developer ID Application:' | head -n1 | awk '{print $2}')}"
if [[ -z "$identity" ]]; then
  echo "error: releases need a Developer ID Application certificate" >&2
  exit 1
fi
if [[ -n "${NOTARY_KEY_PATH:-}" ]]; then
  notary=(--key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER")
else
  notary=(--keychain-profile annuncia)
fi
if [[ -n "${SPARKLE_KEY_FILE:-}" ]]; then
  sparkle_key=(--ed-key-file "$SPARKLE_KEY_FILE")
else
  sparkle_key=(--account annuncia)
fi

app="$(ANNUNCIA_SIGN_IDENTITY="$identity" ANNUNCIA_TIMESTAMP=1 scripts/bundle.sh release | tail -n1)"
out="target/release/dist"
rm -rf "$out"
mkdir -p "$out"
zip="$out/Annuncia.zip"

# Notarize, then staple the ticket so Gatekeeper accepts the app offline, and zip it again.
ditto -c -k --keepParent "$app" "$zip"
xcrun notarytool submit "$zip" "${notary[@]}" --wait
xcrun stapler staple "$app"
rm "$zip"
ditto -c -k --keepParent "$app" "$zip"

# The download has to exist before the feed points at it.
gh release create "$tag" "$zip" --repo "$repo" --title "Annuncia $version" --generate-notes --verify-tag

# The feed entry: Sparkle checks the EdDSA signature against SUPublicEDKey before installing.
signature="$("$(scripts/sparkle.sh)/bin/sign_update" "${sparkle_key[@]}" "$zip")"
item="    <item>
      <title>Version $version</title>
      <pubDate>$(LC_ALL=C date -u "+%a, %d %b %Y %H:%M:%S +0000")</pubDate>
      <sparkle:version>$version</sparkle:version>
      <sparkle:shortVersionString>$version</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>15.0</sparkle:minimumSystemVersion>
      <enclosure url=\"https://github.com/$repo/releases/download/$tag/Annuncia.zip\" type=\"application/octet-stream\" $signature/>
    </item>"

# The tag may be behind main by now, so the feed entry goes on main's tip, newest first. That
# happens on a detached checkout, leaving any local branch alone.
start="$(git symbolic-ref -q --short HEAD || git rev-parse HEAD)"
git fetch -q origin main
git checkout -q --detach origin/main
ITEM="$item" perl -0pi -e 's/(<!-- releases -->\n)/$1$ENV{ITEM}\n/' "$feed"
git add "$feed"
git commit -q -m "Add $version to the update feed"
git push -q origin HEAD:main
git checkout -q "$start"
echo "Released $version"
