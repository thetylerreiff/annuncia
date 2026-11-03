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

app="target/$profile/Annuncia.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "target/$profile/annuncia" "$app/Contents/MacOS/Annuncia"

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
  <key>CFBundleIdentifier</key><string>dev.annuncia.Annuncia</string>
  <key>CFBundleName</key><string>Annuncia</string>
  <key>CFBundleDisplayName</key><string>Annuncia</string>
  <key>CFBundleExecutable</key><string>Annuncia</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>15.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSCameraUsageDescription</key><string>Annuncia shows your camera in a bubble on your recordings.</string>
  <key>NSCameraUseContinuityCameraDeviceType</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>Annuncia records your voiceover.</string>
</dict>
</plist>
PLIST

# Sign with a real identity so macOS keeps the privacy grants across rebuilds; an ad-hoc
# signature changes with every build and leaves Screen Recording/Camera grants stale.
# Override with ANNUNCIA_SIGN_IDENTITY (a name or SHA-1 from `security find-identity -p codesigning`).
identity="${ANNUNCIA_SIGN_IDENTITY:-$(security find-identity -v -p codesigning \
  | grep -E '"(Apple Development|Developer ID Application):' | head -n1 | awk '{print $2}')}"
if [[ -z "$identity" ]]; then
  echo "warning: no signing identity found; signing ad-hoc (permissions reset on every rebuild)" >&2
  identity="-"
fi
codesign --force --sign "$identity" "$app"
echo "$app"
