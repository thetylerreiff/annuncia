#!/usr/bin/env bash
# Publishes Annuncia.app on an update channel: builds and notarizes it, uploads it to GitHub and
# points that channel's feed at it.
#
#   scripts/release.sh          Stable: the tagged version in Cargo.toml, added to the feed on
#                               the site (site/public/appcast.xml, committed to main). The
#                               Release workflow runs it when a v* tag is pushed:
#                                 git tag v0.2.0 && git push origin v0.2.0
#   scripts/release.sh nightly  Nightly: HEAD as version.ANNUNCIA_BUILD, replacing the last one
#                               on the rolling `nightly` prerelease, which also holds its feed.
#                               The Nightly workflow runs it.
#
# It also runs locally. Credentials come from the environment in CI, or from the login keychain:
#   Notarization  NOTARY_KEY_PATH, NOTARY_KEY_ID, NOTARY_ISSUER (an App Store Connect API key),
#                 or a profile saved with `xcrun notarytool store-credentials annuncia`
#   Sparkle key   SPARKLE_KEY_FILE (exported with `generate_keys --account annuncia -x`),
#                 or the keychain item generate_keys made
#   GitHub        GH_TOKEN, or a signed-in `gh`
set -euo pipefail

cd "$(dirname "$0")/.."
channel="${1:-stable}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
repo="thetylerreiff/annuncia"

if [[ -n "$(git status --porcelain)" ]]; then
  echo "error: commit or stash your changes first" >&2
  exit 1
fi
case "$channel" in
  stable)
    tag="v$version"
    bundle_version="$version"
    if [[ "$(git rev-parse -q --verify "refs/tags/$tag^{commit}" || true)" != "$(git rev-parse HEAD)" ]]; then
      echo "error: HEAD isn't tagged $tag; bump Cargo.toml, then tag and push" >&2
      exit 1
    fi
    ;;
  nightly)
    tag="nightly"
    bundle_version="$version.${ANNUNCIA_BUILD:?nightly releases need a build number in ANNUNCIA_BUILD}"
    ;;
  *)
    echo "usage: scripts/release.sh [nightly]" >&2
    exit 1
    ;;
esac
identity="${ANNUNCIA_SIGN_IDENTITY:-$(security find-identity -v -p codesigning \
  | awk '/"Developer ID Application:/ { print $2; exit }')}"
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

app="$(ANNUNCIA_SIGN_IDENTITY="$identity" ANNUNCIA_TIMESTAMP=1 ANNUNCIA_CHANNEL="$channel" \
  scripts/bundle.sh release | tail -n1)"
out="target/release/dist"
rm -rf "$out"
mkdir -p "$out"
# Nightly zips are named by version: the feed and the zip upload separately, and an old feed
# must never point at a new zip it has no signature for. The disk images keep fixed names, so
# the website's links (releases/latest/download/Annuncia.dmg, and releases/download/nightly/
# Annuncia-Nightly.dmg) download the newest directly.
if [[ "$channel" == nightly ]]; then
  name="Annuncia-$bundle_version.zip"
  dmg="$out/Annuncia-Nightly.dmg"
else
  name="Annuncia.zip"
  dmg="$out/Annuncia.dmg"
fi
zip="$out/$name"

# Notarize, then staple the ticket so Gatekeeper accepts the app offline, and zip it again.
ditto -c -k --keepParent "$app" "$zip"
xcrun notarytool submit "$zip" "${notary[@]}" --wait
xcrun stapler staple "$app"
rm "$zip"
ditto -c -k --keepParent "$app" "$zip"

# The disk image the website offers, notarized and stapled on its own so it opens offline too.
ANNUNCIA_SIGN_IDENTITY="$identity" scripts/dmg.sh "$app" "$dmg" >/dev/null
xcrun notarytool submit "$dmg" "${notary[@]}" --wait
xcrun stapler staple "$dmg"

# The feed entry: Sparkle checks the EdDSA signature against SUPublicEDKey before installing.
signature="$("$(scripts/sparkle.sh)/bin/sign_update" "${sparkle_key[@]}" "$zip")"
item="    <item>
      <title>Version $bundle_version</title>
      <pubDate>$(LC_ALL=C date -u "+%a, %d %b %Y %H:%M:%S +0000")</pubDate>
      <sparkle:version>$bundle_version</sparkle:version>
      <sparkle:shortVersionString>$(plutil -extract CFBundleShortVersionString raw "$app/Contents/Info.plist")</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>15.0</sparkle:minimumSystemVersion>
      <enclosure url=\"https://github.com/$repo/releases/download/$tag/$name\" type=\"application/octet-stream\" $signature/>
    </item>"

if [[ "$channel" == nightly ]]; then
  # Move the rolling tag to this commit, then swap in the new build before the feed that names it.
  sha="$(git rev-parse HEAD)"
  git tag -f nightly "$sha"
  git push -q -f origin refs/tags/nightly
  notes="Built from $sha on $(date -u +%Y-%m-%d). Nightly builds update themselves every night main changes."
  if gh release view nightly --repo "$repo" >/dev/null 2>&1; then
    gh release edit nightly --repo "$repo" --notes "$notes"
  else
    gh release create nightly --repo "$repo" --prerelease --title "Annuncia Nightly" --notes "$notes"
  fi
  gh release upload nightly "$zip" "$dmg" --repo "$repo" --clobber
  cat > "$out/appcast.xml" <<XML
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>Annuncia Nightly</title>
$item
  </channel>
</rss>
XML
  gh release upload nightly "$out/appcast.xml" --repo "$repo" --clobber
  for old in $(gh release view nightly --repo "$repo" --json assets \
    --jq '.assets[].name | select(startswith("Annuncia-") and . != "Annuncia-Nightly.dmg")'); do
    [[ "$old" == "$name" ]] || gh release delete-asset nightly "$old" --repo "$repo" --yes
  done
  echo "Released nightly $bundle_version"
  exit 0
fi

# The download has to exist before the feed points at it.
gh release create "$tag" "$zip" "$dmg" --repo "$repo" --title "Annuncia $version" --generate-notes --verify-tag

# The tag may be behind main by now, so the feed entry goes on main's tip, newest first. That
# happens on a detached checkout, leaving any local branch alone.
start="$(git symbolic-ref -q --short HEAD || git rev-parse HEAD)"
git fetch -q origin main
git checkout -q --detach origin/main
ITEM="$item" perl -0pi -e 's/(<!-- releases -->\n)/$1$ENV{ITEM}\n/' site/public/appcast.xml
git add site/public/appcast.xml
git commit -q -m "Add $version to the update feed"
git push -q origin HEAD:main
git checkout -q "$start"
echo "Released $version"
