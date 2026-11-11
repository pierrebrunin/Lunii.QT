#!/usr/bin/env bash
# Crée une fausse Lunii v2 avec des histoires inventées, puis lance l'application.
# Usage : tools/fake-lunii.sh [--no-run] [dossier]   (dossier temporaire par défaut)
set -euo pipefail

run=1
if [[ ${1:-} == --no-run ]]; then
    run=0
    shift
fi
out=${1:-$(mktemp -d -t fake-lunii.XXXXXX)}
cd "$(dirname "$0")/.."

cargo run -q -p lunii-core --example fake_lunii -- "$out"

export LUNII_DEVICES="$out/lunii" LUNII_CFG_DIR="$out/cfg" LUNII_NO_UPDATE_CHECK=1
echo "Fausse Lunii : $out"
echo "Relancer : LUNII_DEVICES=$LUNII_DEVICES LUNII_CFG_DIR=$LUNII_CFG_DIR LUNII_NO_UPDATE_CHECK=1 cargo run -p lunii-gui"
if ((run)); then
    exec cargo run -q -p lunii-gui
fi
