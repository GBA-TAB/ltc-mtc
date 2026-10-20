#!/bin/bash
# Runs ltc-mtc on the loopback with LTC playing in a loop, for looking at the web UI.
# Usage: [LTC_DEMO_PORT=8088] scripts/ui-demo.sh start|stop   (after loopback-test.sh created the config and WAV)
DIR=${LTC_TEST_DIR:-$HOME/ltc-mtc-test}
PORT=${LTC_DEMO_PORT:-8088}
case "$1" in
start)
  ./target/release/ltc-mtc gen "$DIR/loop.wav" --start 00:59:52:00 --fps 25 --seconds 600 --level -18 >/dev/null
  nohup ./target/release/ltc-mtc --config "$DIR/config.json" --listen 0.0.0.0:$PORT > "$DIR/demo.log" 2>&1 &
  echo $! > "$DIR/demo.pids"
  sleep 1
  # Plays the 10-minute file over and over (00:59:52:00 to 01:09:52:00).
  nohup bash -c "while aplay -q -D plughw:CARD=Loopback,DEV=0 '$DIR/loop.wav'; do :; done" >/dev/null 2>&1 &
  echo $! >> "$DIR/demo.pids"
  cat "$DIR/demo.pids";;
stop)
  # Note the loop's aplay before stopping the loop (afterwards it has another parent), stop
  # the loop (bash acts on the signal once aplay ends), then aplay, then ltc-mtc.
  LOOP=$(sed -n 2p "$DIR/demo.pids")
  PLAYER=$(pgrep -P "$LOOP")
  kill "$LOOP"; [ -n "$PLAYER" ] && kill $PLAYER
  kill "$(head -1 "$DIR/demo.pids")"; rm -f "$DIR/demo.pids";;
esac
