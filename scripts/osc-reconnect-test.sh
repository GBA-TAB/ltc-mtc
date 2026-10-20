#!/bin/bash
# TCP target down at start, coming up, restarting: ltc-mtc must connect and reconnect by itself.
# Usage: scripts/osc-reconnect-test.sh [BINARY]
set -u
BIN=${1:-./target/release/ltc-mtc}
DIR=${LTC_TEST_DIR:-$HOME/ltc-mtc-test}
PORT=${LTC_TEST_PORT:-8090}
cd "$(dirname "$0")/.."
mkdir -p "$DIR"
cat > "$DIR/reconnect.json" <<JSON
{ "audio_device": "null", "mtc_enabled": false,
  "osc_targets": [ { "name": "show", "host": "127.0.0.1", "port": 9005, "protocol": "tcp" } ],
  "events": [] }
JSON
"$BIN" --config "$DIR/reconnect.json" --listen 127.0.0.1:$PORT > "$DIR/reconnect.log" 2>&1 & APP=$!
st() { curl -s 127.0.0.1:$PORT/api/status | python3 -c "import json,sys; t=json.load(sys.stdin)['osc'][0]; print('   status:', 'ok' if t['ok'] else 'ERROR', t['error'] or '', '| sent', t['sent'])"; }
fire() { curl -s -X POST --data "{\"commands\":[{\"type\":\"osc\",\"target\":\"show\",\"address\":\"$1\"}]}" 127.0.0.1:$PORT/api/fire >/dev/null; }
sleep 1.5
echo "1. target down"; st
python3 -u scripts/osc-listen.py tcp 9005 > "$DIR/rc1.txt" 2>&1 & L=$!
sleep 2.5
echo "2. target up (reconnect within 2 s)"; st
fire /one; sleep 0.3; st
kill $L; wait $L 2>/dev/null; sleep 0.5
echo "3. target stopped"; st
python3 -u scripts/osc-listen.py tcp 9005 > "$DIR/rc2.txt" 2>&1 & L=$!
sleep 2.5
echo "4. target restarted"; st
fire /two; sleep 0.3; st
kill $L $APP; wait 2>/dev/null
echo "--- first listener";  cat "$DIR/rc1.txt"
echo "--- second listener"; cat "$DIR/rc2.txt"
echo "--- ltc-mtc"; grep osc "$DIR/reconnect.log"
