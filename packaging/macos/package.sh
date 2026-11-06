#!/usr/bin/env bash
# macOS package: universal (Apple Silicon + Intel) `luniiQt.app` in a DMG.
#
#   cargo build --release --target aarch64-apple-darwin
#   cargo build --release --target x86_64-apple-darwin
#   packaging/macos/package.sh v3.1.4 dist
#
# Run from the repository root. The application is signed ad hoc only (no Apple developer
# certificate): Gatekeeper asks for a confirmation at the first launch, see the
# "macOS" section of the README.
set -euo pipefail

VERSION=${1:?usage: package.sh <version> [output dir]}
OUT=${2:-dist}
cd "$(dirname "$0")/../.."
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

# DMG contents: the application (+ the Applications link)
SRC="$STAGE/dmg"
APP="$SRC/luniiQt.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
for bin in lunii-qt lunii-cli; do
    lipo -create -output "$APP/Contents/MacOS/$bin" \
        "target/aarch64-apple-darwin/release/$bin" \
        "target/x86_64-apple-darwin/release/$bin"
done
lipo -info "$APP/Contents/MacOS/lunii-qt"
sed "s/@VERSION@/${VERSION#v}/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
cp res/dmg_icon.icns "$APP/Contents/Resources/AppIcon.icns"
cp LICENSE "$APP/Contents/Resources/"

# ad hoc signature: required for arm64 executables modified after linking (lipo)
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"

DMG="$OUT/Lunii.Qt-$VERSION-MacOs-universal.dmg"
rm -f "$DMG"
if command -v create-dmg >/dev/null; then
    # same layout as the previous releases: application on the left,
    # Applications link on the right
    create-dmg \
        --volname "luniiQt-$VERSION Installer" \
        --volicon res/dmg_icon.icns \
        --background res/dmg_background.png \
        --window-pos 200 120 \
        --window-size 600 425 \
        --icon-size 70 \
        --icon luniiQt.app 185 185 \
        --app-drop-link 415 185 \
        --format UDZO \
        "$DMG" "$SRC"
else
    ln -s /Applications "$SRC/Applications"
    hdiutil create -volname "luniiQt-$VERSION" -srcfolder "$SRC" -ov -format UDZO "$DMG"
fi
echo "$DMG"
