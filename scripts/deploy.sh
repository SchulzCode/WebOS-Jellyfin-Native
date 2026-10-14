#!/usr/bin/env bash
# Package (scripts/package.sh) and install on the TV through the Homebrew Channel. Doesn't launch the app.
set -euo pipefail
. "$(dirname "$0")/env.sh"
"$ROOT/scripts/package.sh"
IPK=$(ls "$ROOT"/build/out/*.ipk); NAME=$(basename "$IPK"); HASH=$(sha256sum "$IPK" | cut -d' ' -f1)
VER=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["version"])' "$ROOT/pkg/appinfo.json")
python3 -m http.server "$PC_HTTP_PORT" --bind "$PC_IP" --directory "$ROOT/build/out" >/dev/null 2>&1 & SRV=$!
trap 'kill $SRV 2>/dev/null || true' EXIT
sleep 1
OUT=$(tvl "timeout 150 luna-send -i -f luna://org.webosbrew.hbchannel.service/install '{\"ipkUrl\":\"http://$PC_IP:$PC_HTTP_PORT/$NAME\",\"ipkHash\":\"$HASH\",\"subscribe\":true}'" 2>&1 || true)
echo "$OUT" | grep -q '"finished": true' && echo "installed $VER" || { echo "INSTALL FAILED"; echo "$OUT" | tail -25; exit 1; }
