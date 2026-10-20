#!/bin/bash
# End-to-end test with snd-aloop and Midi Through: plays LTC into the loopback, runs ltc-mtc
# on the loopback capture, and records what arrives on Midi Through with aseqdump.
# Usage: scripts/loopback-test.sh [BINARY] [FPS]   (needs sudo for modprobe)
set -u
BIN=${1:-./target/release/ltc-mtc}
FPS=${2:-25}
DIR=${LTC_TEST_DIR:-$HOME/ltc-mtc-test}
PORT=${LTC_TEST_PORT:-8090}
mkdir -p "$DIR"
lsmod | grep -q snd_aloop || sudo modprobe snd-aloop || exit 1
sleep 1
cat > "$DIR/config.json" <<JSON
{ "audio_device": "hw:CARD=Loopback,DEV=1", "audio_channel": 1, "sample_rate": 48000,
  "midi_output": "Midi Through:Midi Through Port-0", "mtc_enabled": true, "fps_mode": "auto",
  "events": [
    { "name": "start", "time": "00:59:55:00", "commands": [ { "type": "pc", "channel": 1, "program": 5 } ] },
    { "name": "cue 2", "time": "00:59:58:12", "commands": [ { "type": "cc", "channel": 2, "controller": 7, "value": 100 },
                                                             { "type": "pc", "channel": 2, "program": 3 } ] },
    { "name": "off", "time": "00:59:59:00", "enabled": false, "commands": [ { "type": "cc", "channel": 3, "controller": 1, "value": 1 } ] },
    { "name": "after", "time": "01:00:01:00", "commands": [ { "type": "cc", "channel": 16, "controller": 64, "value": 0 } ] }
  ] }
JSON
"$BIN" gen "$DIR/ltc.wav" --start 00:59:50:00 --fps "$FPS" --seconds 13 --level -18
"$BIN" --config "$DIR/config.json" --listen 127.0.0.1:$PORT > "$DIR/ltc-mtc.log" 2>&1 &
APP=$!
aseqdump -p "Midi Through" > "$DIR/dump.txt" 2>&1 &
DUMP=$!
sleep 2
aplay -q -D plughw:CARD=Loopback,DEV=0 "$DIR/ltc.wav" &
PLAY=$!
sleep 7
curl -s 127.0.0.1:$PORT/api/status > "$DIR/status-running.json"
wait $PLAY
sleep 1
curl -s 127.0.0.1:$PORT/api/status > "$DIR/status-after.json"
kill $APP $DUMP
wait 2>/dev/null
echo "--- ltc-mtc log"; cat "$DIR/ltc-mtc.log"
echo "--- status while running"; python3 -c "import json,sys; s=json.load(open(sys.argv[1])); print({k:s[k] for k in ['tc','running','fps','speed','level_db','midi_port','audio_error','midi_error']})" "$DIR/status-running.json"
echo "--- status after"; python3 -c "import json,sys; s=json.load(open(sys.argv[1])); print({k:s[k] for k in ['tc','running','frames_decoded']}); [print(' ', l['tc'], l['name'], l['commands']) for l in s['log']]" "$DIR/status-after.json"
echo "--- MIDI received"
grep -c "MTC quarter" "$DIR/dump.txt" | sed 's/^/quarter frames: /'
grep -E "System exclusive|Control change|Program change" "$DIR/dump.txt"
