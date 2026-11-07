#!/usr/bin/env bash
# Linux packages: `.tar.gz` (both binaries) and AppImage (graphical interface).
#
#   cargo build --release
#   packaging/linux/package.sh v4.0.0 dist
#
# Run from the repository root. appimagetool is taken from $APPIMAGETOOL, the PATH, or
# downloaded. In containers without FUSE, set APPIMAGE_EXTRACT_AND_RUN=1.
set -euo pipefail

VERSION=${1:?usage: package.sh <version> [output dir]}
OUT=${2:-dist}
cd "$(dirname "$0")/../.."
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
ARCH=x86_64
NAME="Lunii-$VERSION-Linux-$ARCH"
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

for bin in lunii lunii-cli; do
    [[ -x target/release/$bin ]] || { echo "target/release/$bin missing: cargo build --release" >&2; exit 1; }
done

# --- archive -----------------------------------------------------------------
mkdir -p "$STAGE/$NAME"
cp target/release/lunii target/release/lunii-cli "$STAGE/$NAME/"
cp packaging/linux/lunii.desktop "$STAGE/$NAME/"
cp lunii-gui/ui/img/lunii.png "$STAGE/$NAME/lunii.png"
cp LICENSE "$STAGE/$NAME/"
tar -C "$STAGE" -czf "$OUT/$NAME.tar.gz" "$NAME"
echo "$OUT/$NAME.tar.gz"

# --- AppImage ----------------------------------------------------------------
APPDIR="$STAGE/Lunii.AppDir"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" \
    "$APPDIR/usr/share/icons/hicolor/256x256/apps"
cp target/release/lunii target/release/lunii-cli "$APPDIR/usr/bin/"
cp packaging/linux/lunii.desktop "$APPDIR/"
cp packaging/linux/lunii.desktop "$APPDIR/usr/share/applications/"
cp lunii-gui/ui/img/lunii.png "$APPDIR/lunii.png"
cp lunii-gui/ui/img/lunii.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/lunii.png"
ln -s lunii.png "$APPDIR/.DirIcon"
cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
# `Lunii.AppImage cli <args>` runs the command line tool
HERE=$(dirname "$(readlink -f "$0")")
if [ "$1" = "cli" ]; then
    shift
    exec "$HERE/usr/bin/lunii-cli" "$@"
fi
exec "$HERE/usr/bin/lunii" "$@"
EOF
chmod +x "$APPDIR/AppRun"

TOOL=${APPIMAGETOOL:-$(command -v appimagetool || true)}
if [[ -z "$TOOL" ]]; then
    TOOL="$STAGE/appimagetool"
    curl -fsSL -o "$TOOL" \
        "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
    chmod +x "$TOOL"
fi
ARCH=$ARCH "$TOOL" --no-appstream "$APPDIR" "$OUT/Lunii-$VERSION-$ARCH.AppImage"
echo "$OUT/Lunii-$VERSION-$ARCH.AppImage"
