#!/bin/bash
# OSC end-to-end test with snd-aloop: LTC in, OSC out to scripts/osc-listen.py.
# Targets: an IP address, a host name (localhost), and a name that does not resolve.
# Events 1 s apart check the timing from the receive timestamps.
# Usage: scripts/osc-test.sh [BINARY] [PROTOCOL]   (PROTOCOL: udp, tcp or tcp-len)
set -u
BIN=${1:-./target/release/ltc-mtc}
PROTO=${2:-udp}
DIR=${LTC_TEST_DIR:-$HOME/ltc-mtc-test}
PORT=${LTC_TEST_PORT:-8090}
cd "$(dirname "$0")/.."
mkdir -p "$DIR"
lsmod | grep -q snd_aloop || sudo modprobe snd-aloop || exit 1
"$BIN" gen "$DIR/ltc.wav" --start 00:59:54:00 --fps 25 --seconds 7 --level -18 >/dev/null
python3 -u scripts/osc-listen.py "$PROTO" 9000 > "$DIR/osc-a.txt" 2>&1 & LA=$!
python3 -u scripts/osc-listen.py "$PROTO" 9001 > "$DIR/osc-b.txt" 2>&1 & LB=$!
cat > "$DIR/osc.json" <<JSON
{ "audio_device": "hw:CARD=Loopback,DEV=1", "audio_channel": 1, "fps_mode": "auto",
  "midi_output": "Midi Through:Midi Through Port-0", "mtc_enabled": false,
  "osc_targets": [
    { "name": "ip", "host": "127.0.0.1", "port": 9000, "protocol": "$PROTO" },
    { "name": "byname", "host": "localhost", "port": 9001, "protocol": "$PROTO" },
    { "name": "broken", "host": "no-such-host.invalid", "port": 9002, "protocol": "$PROTO" } ],
  "events": [
    { "name": "go", "time": "00:59:56:00", "commands": [
      { "type": "osc", "target": "ip", "address": "/cue/1/go" },
      { "type": "osc", "target": "byname", "address": "/mix/fader", "args": [
        { "type": "float", "value": 0.75 }, { "type": "int", "value": -3 },
        { "type": "string", "value": "hé llo" }, { "type": "bool", "value": true }, { "type": "bool", "value": false } ] },
      { "type": "osc", "target": "broken", "address": "/never" },
      { "type": "pc", "channel": 1, "program": 7 } ] },
    { "name": "plus 1 s", "time": "00:59:57:00", "commands": [ { "type": "osc", "target": "ip", "address": "/cue/2/go", "args": [ { "type": "int", "value": 2 } ] } ] },
    { "name": "plus 2 s 12 f", "time": "00:59:58:12", "commands": [ { "type": "osc", "target": "ip", "address": "/cue/3/go" } ] } ] }
JSON
"$BIN" --config "$DIR/osc.json" --listen 127.0.0.1:$PORT > "$DIR/osc-app.log" 2>&1 & APP=$!
sleep 1.5
aplay -q -D plughw:CARD=Loopback,DEV=0 "$DIR/ltc.wav"
sleep 0.5
curl -s 127.0.0.1:$PORT/api/status > "$DIR/osc-status.json"
kill $APP; sleep 0.3; kill $LA $LB; wait 2>/dev/null
echo "--- ltc-mtc"; grep -v "^note" "$DIR/osc-app.log"
echo "--- target status"
python3 -c "import json,sys; [print(' ', t) for t in json.load(open(sys.argv[1]))['osc']]" "$DIR/osc-status.json"
echo "--- received on 9000 (ip)"; cat "$DIR/osc-a.txt"
echo "--- received on 9001 (byname)"; cat "$DIR/osc-b.txt"
python3 - "$DIR/osc-a.txt" <<'PY'
import json, sys
t = {m["address"]: m["t"] for m in map(json.loads, open(sys.argv[1])) if "address" in m}
if all(k in t for k in ("/cue/1/go", "/cue/2/go", "/cue/3/go")):
    d1 = (t["/cue/2/go"] - t["/cue/1/go"]) * 1000 - 1000
    d2 = (t["/cue/3/go"] - t["/cue/1/go"]) * 1000 - 2480
    print(f"--- timing: cue 2 {d1:+.1f} ms, cue 3 {d2:+.1f} ms from the timecode (one frame = 40 ms)")
PY
