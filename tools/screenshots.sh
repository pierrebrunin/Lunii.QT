#!/usr/bin/env bash
# Refait les captures du README (res/screenshot_{main,debug,lost,interface}.png)
# sur une fausse Lunii (tools/fake-lunii.sh). Besoin de Python 3 avec Pillow.
# Usage : tools/screenshots.sh [dossier de travail]   (temporaire par défaut)
set -euo pipefail

work=${1:-$(mktemp -d -t lunii-shots.XXXXXX)}
cd "$(dirname "$0")/.."

cargo build -q -p lunii-gui --features screenshot
tools/fake-lunii.sh --no-run "$work/fake" >/dev/null

LUNII_DEVICES="$work/fake/lunii" LUNII_CFG_DIR="$work/fake/cfg" \
LUNII_NO_UPDATE_CHECK=1 LUNII_LANG=fr LUNII_SCREENSHOT="$work/shots" \
    target/debug/lunii >/dev/null 2>&1

cp "$work/shots/fr-02-details.png" res/screenshot_main.png
cp "$work/shots/fr-10-log.png" res/screenshot_debug.png
cp "$work/shots/fr-10-lost-stories.png" res/screenshot_lost.png
python3 tools/annotate_interface.py "$work/shots/fr-05-sizes.png" res/screenshot_interface.png

echo "Captures mises à jour dans res/ (toutes les captures : $work/shots)"
