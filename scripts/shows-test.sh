#!/bin/bash
# Shows and cues start/stop through the API, with LTC on snd-aloop:
# save as / save / load / overwrite protection / bad names / upload / delete, the start/stop
# switch surviving a restart, and stopped cues not firing.
# Usage: scripts/shows-test.sh [BINARY]
set -u
BIN=${1:-./target/release/ltc-mtc}
DIR=${LTC_TEST_DIR:-$HOME/ltc-mtc-test}/shows-test
PORT=${LTC_TEST_PORT:-8090}
cd "$(dirname "$0")/.."
rm -rf "$DIR"; mkdir -p "$DIR"
lsmod | grep -q snd_aloop || sudo modprobe snd-aloop || exit 1
"$BIN" gen "$DIR/ltc.wav" --start 00:59:54:00 --fps 25 --seconds 4 --level -18 >/dev/null
U=127.0.0.1:$PORT
start() { "$BIN" --config "$DIR/config.json" --listen $U >> "$DIR/app.log" 2>&1 & APP=$!; sleep 1.2; }
call() { curl -s -w ' [%{http_code}]' -X "$1" ${3:+--data "$3"} "$U$2"; echo; }
cue() { echo "{\"audio_device\":\"hw:CARD=Loopback,DEV=1\",\"midi_output\":\"Midi Through:Midi Through Port-0\",\"mtc_enabled\":false,
  \"events\":[{\"name\":\"$1\",\"time\":\"00:59:56:00\",\"commands\":[{\"type\":\"pc\",\"channel\":1,\"program\":$2}]}]}"; }
fired() { curl -s $U/api/status | python3 -c "import json,sys; s=json.load(sys.stdin); print('   show:', s['show'], '| armed:', s['armed'], '| fired:', [l['name'] for l in s['log']])"; }
play() { aplay -q -D plughw:CARD=Loopback,DEV=0 "$DIR/ltc.wav"; sleep 0.3; }
start
echo "1. new device, no show"; fired
echo "2. apply cue A, save without a show (must ask for a name)"; call PUT /api/config "$(cue A 1)" >/dev/null; call POST /api/shows/save '{}'
echo "3. save as 'Act 1'"; call POST /api/shows/save '{"name":"Act 1"}'
echo "4. apply cue B, save as 'Act 2', then 'Act 1' without overwrite"; call PUT /api/config "$(cue B 2)" >/dev/null
call POST /api/shows/save '{"name":"Act 2"}'; call POST /api/shows/save '{"name":"Act 1"}'
echo "5. bad names"; call POST /api/shows/save '{"name":"../etc/passwd"}'; call POST /api/shows/save '{"name":".hidden"}'
echo "6. list"; call GET /api/shows
echo "7. load 'Act 1' and play: cue A fires"; call POST /api/shows/load '{"name":"Act 1"}' | cut -c1-60; play; fired
echo "8. stop cues, play again: nothing more fires"; call POST /api/armed '{"armed":false}'; play; fired
echo "9. restart ltc-mtc: still stopped, still 'Act 1'"; kill $APP; wait $APP 2>/dev/null; start; fired
echo "10. start cues, play: cue A fires"; call POST /api/armed '{"armed":true}'; play; fired
echo "11. upload a show, refuse the same name, delete it"
call POST /api/shows/upload "{\"name\":\"Zürich (Probe)\",\"config\":$(cue Z 9)}"
call POST /api/shows/upload "{\"name\":\"Zürich (Probe)\",\"config\":$(cue Z 9)}"
call POST /api/shows/get '{"name":"Zürich (Probe)"}' | head -c 80; echo
call POST /api/shows/delete '{"name":"Zürich (Probe)"}'
echo "12. delete the current show"; call POST /api/shows/delete '{"name":"Act 1"}'; fired
kill $APP; wait 2>/dev/null
echo "--- files"; ls -1 "$DIR" "$DIR/shows"; cat "$DIR/state.json"; echo
echo "--- log"; grep -E "^(show|cues|fire)" "$DIR/app.log"
