#!/usr/bin/env bash
# Builds "Notch Assistant.app" from the release binary, ad-hoc signed.
#
#   scripts/bundle.sh [out-dir]        default: target/bundle.noindex
#
# The bundle is an agent app (LSUIElement): no Dock icon, as when run from the terminal.
set -euo pipefail
cd "$(dirname "$0")/.."

out=${1:-target/bundle.noindex}
app="$out/Notch Assistant.app"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
id=${BUNDLE_ID:-io.github.shmileee.notch-assistant}

cargo build --release --quiet

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/notch-assistant "$app/Contents/MacOS/notch-assistant"
cp assets/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Notch Assistant</string>
  <key>CFBundleDisplayName</key><string>Notch Assistant</string>
  <key>CFBundleIdentifier</key><string>$id</string>
  <key>CFBundleExecutable</key><string>notch-assistant</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSUIElement</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc: enough to run locally and to register as a login item. Not notarized.
codesign --force --sign - --identifier "$id" "$app" >/dev/null 2>&1
echo "$app"
