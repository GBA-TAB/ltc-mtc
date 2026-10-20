#!/bin/bash
# MIDI routing test with snd-aloop: default commands -> Midi Through, MTC -> aseqdump A,
# one command with its own output -> aseqdump B. Run twice: MTC on, then MTC off.
# Usage: scripts/routing-test.sh [BINARY]
set -u
BIN=${1:-./target/release/ltc-mtc}
DIR=${LTC_TEST_DIR:-$HOME/ltc-mtc-test}
PORT=${LTC_TEST_PORT:-8090}
mkdir -p "$DIR"
lsmod | grep -q snd_aloop || sudo modprobe snd-aloop || exit 1
"$BIN" gen "$DIR/ltc.wav" --start 00:59:54:00 --fps 25 --seconds 6 --level -18 >/dev/null

for MTC in true false; do
  # The aseqdump clients all share a name, so start them one by one and note each new
  # client number (they are not numbered in start order when started together).
  clients() { aconnect -o | grep -E "^client [0-9]+: 'aseqdump'" | sed -E 's/client ([0-9]+):.*/\1/' | sort -n; }
  newclient() { comm -13 <(echo "$1") <(clients) | head -1; }
  before=$(clients); aseqdump > "$DIR/a.txt" 2>&1 & A=$!; sleep 0.4; CA=$(newclient "$before")
  before=$(clients); aseqdump > "$DIR/b.txt" 2>&1 & B=$!; sleep 0.4; CB=$(newclient "$before")
  aseqdump -p 14:0 > "$DIR/through.txt" 2>&1 & T=$!
  sleep 0.4
  cat > "$DIR/routing.json" <<JSON
{ "audio_device": "hw:CARD=Loopback,DEV=1", "audio_channel": 1, "fps_mode": "auto",
  "midi_output": "Midi Through:Midi Through Port-0", "mtc_enabled": $MTC, "mtc_output": "$CA:0",
  "events": [ { "name": "two outputs", "time": "00:59:56:00", "commands": [
      { "type": "pc", "channel": 1, "program": 11 },
      { "type": "cc", "channel": 5, "controller": 20, "value": 99, "output": "$CB:0" } ] } ] }
JSON
  "$BIN" --config "$DIR/routing.json" --listen 127.0.0.1:$PORT > "$DIR/routing.log" 2>&1 & APP=$!
  sleep 1.5
  aplay -q -D plughw:CARD=Loopback,DEV=0 "$DIR/ltc.wav"
  sleep 0.5
  curl -s 127.0.0.1:$PORT/api/status | python3 -c "import json,sys; s=json.load(sys.stdin); print('status: commands ->', s['midi_port'], '| mtc ->', s['mtc_port'], '| error', s['midi_error'])"
  kill $APP $A $B $T; wait 2>/dev/null
  echo "=== MTC $MTC (A = $CA:0 MTC, B = $CB:0 routed CC)"
  for f in a b through; do
    printf '%-8s QF %4s  SysEx %2s  CC %s  PC %s\n' $f \
      "$(grep -c 'MTC quarter' $DIR/$f.txt)" "$(grep -c 'System exclusive' $DIR/$f.txt)" \
      "$(grep 'Control change' $DIR/$f.txt | sed -E 's/ +/ /g' | cut -d' ' -f4- | tr '\n' ';')" \
      "$(grep 'Program change' $DIR/$f.txt | sed -E 's/ +/ /g' | cut -d' ' -f4- | tr '\n' ';')"
  done
done
