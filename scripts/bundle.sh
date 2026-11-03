#!/usr/bin/env bash
# Builds Annuncia.app. macOS only grants camera/microphone/screen access to a bundled app with
# usage descriptions, so run the bundle rather than the bare binary.
set -euo pipefail

cd "$(dirname "$0")/.."
profile="${1:-release}"
if [[ "$profile" == "release" ]]; then
  cargo build --release
else
  cargo build
fi

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
# Nightlies (ANNUNCIA_CHANNEL=nightly, with a build number in ANNUNCIA_BUILD) sort after the
# release they're built on and before the next: 0.2.0 < 0.2.0.57 < 0.2.1.
channel="${ANNUNCIA_CHANNEL:-stable}"
bundle_version="$version"
display_version="$version"
if [[ "$channel" == nightly ]]; then
  bundle_version="$version.${ANNUNCIA_BUILD:?nightly builds need ANNUNCIA_BUILD}"
  display_version="$version nightly ${ANNUNCIA_BUILD}"
fi

app="target/$profile/Annuncia.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$app/Contents/Frameworks"
cp "target/$profile/annuncia" "$app/Contents/MacOS/Annuncia"

# Sparkle, the updater. The app loads it at runtime, so a bare `cargo run` works without it.
ditto "$(scripts/sparkle.sh)/Sparkle.framework" "$app/Contents/Frameworks/Sparkle.framework"

# App icon: every size macOS asks for, cut from the 1024 px master with the system's own tools.
iconset="$(mktemp -d)/Annuncia.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" assets/icon.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
  sips -z "$((size * 2))" "$((size * 2))" assets/icon.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>co.thetylerreiff.Annuncia</string>
  <key>CFBundleName</key><string>Annuncia</string>
  <key>CFBundleDisplayName</key><string>Annuncia</string>
  <key>CFBundleExecutable</key><string>Annuncia</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleShortVersionString</key><string>$display_version</string>
  <key>CFBundleVersion</key><string>$bundle_version</string>
  <key>LSMinimumSystemVersion</key><string>15.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSCameraUsageDescription</key><string>Annuncia shows your camera in a bubble on your recordings.</string>
  <key>NSCameraUseContinuityCameraDeviceType</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>Annuncia records your voiceover.</string>
  <key>AnnunciaUpdateChannel</key><string>$channel</string>
  <key>SUFeedURL</key><string>https://annuncia.thetylerreiff.co/appcast.xml</string>
  <key>SUPublicEDKey</key><string>ZOkaWi25+g/HEDOZlqbU69ASiOkd6UzdiOrwPYBHDFk=</string>
  <key>SUEnableAutomaticChecks</key><true/>
  <key>SUAutomaticallyUpdate</key><true/>
</dict>
</plist>
PLIST

# Sign with a real identity so macOS keeps the privacy grants across rebuilds; an ad-hoc
# signature changes with every build and leaves Screen Recording/Camera grants stale.
# Override with ANNUNCIA_SIGN_IDENTITY (a name or SHA-1 from `security find-identity -p codesigning`).
# (awk, not grep, so finding none falls through to ad hoc instead of failing under pipefail.)
identity="${ANNUNCIA_SIGN_IDENTITY:-$(security find-identity -v -p codesigning \
  | awk '/"(Apple Development|Developer ID Application):/ { print $2; exit }')}"
if [[ -z "$identity" ]]; then
  echo "warning: no signing identity found; signing ad-hoc (permissions reset on every rebuild)" >&2
  identity="-"
fi
# Notarization needs a secure timestamp, which asks Apple's server; release.sh turns it on.
sign=(codesign --force --sign "$identity" --options runtime)
if [[ "${ANNUNCIA_TIMESTAMP:-}" == 1 ]]; then
  sign+=(--timestamp)
fi
# Inside out, as Sparkle's docs lay out: its helpers, the framework, then the app.
sparkle="$app/Contents/Frameworks/Sparkle.framework/Versions/B"
"${sign[@]}" "$sparkle/XPCServices/Installer.xpc"
"${sign[@]}" --preserve-metadata=entitlements "$sparkle/XPCServices/Downloader.xpc"
"${sign[@]}" "$sparkle/Autoupdate"
"${sign[@]}" "$sparkle/Updater.app"
"${sign[@]}" "$app/Contents/Frameworks/Sparkle.framework"
"${sign[@]}" --entitlements scripts/Annuncia.entitlements "$app"
echo "$app"
