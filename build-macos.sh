#!/bin/sh
# Build ReRust_PCL for macOS and assemble a .app bundle.
#
# Usage (on a Mac, or a macOS CI runner):
#   ./build-macos.sh
#
# Produces: dist/ReRust_PCL.app  (ad-hoc signed when codesign is
# available; NOT notarized — for personal/testing use. Public
# distribution needs a Developer ID + notarization, and the PCL
# licence naming review recorded in PORT-PLAN.md §3/§7.)
set -eu
cd "$(dirname "$0")"

APP_NAME="PCL_ReRust"
DISPLAY_NAME="PCL ReRust"
BUNDLE_ID="com.github.lukeqiang2000.pclrerust"
VERSION="0.1.0"

echo "==> cargo build --release (pcl-ui + pcl-cli)"
cargo build --release -p pcl-ui -p pcl-cli

APP="dist/${APP_NAME}.app"
echo "==> assembling ${APP}"
rm -rf "${APP}"
mkdir -p "${APP}/Contents/MacOS" "${APP}/Contents/Resources"
cp "target/release/pcl-ui" "${APP}/Contents/MacOS/pcl-ui"
cp "target/release/pcl-cli" "${APP}/Contents/MacOS/pcl-cli"

cat > "${APP}/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>${DISPLAY_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleExecutable</key>
    <string>pcl-ui</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

if command -v codesign >/dev/null 2>&1; then
    echo "==> ad-hoc codesign"
    codesign --force --deep --sign - "${APP}" || echo "codesign failed (continuing unsigned)"
fi

echo "==> done: ${APP}"
