#!/bin/bash
# Runs ltc-mtc on the loopback with LTC playing in a loop, for looking at the web UI.
# Usage: scripts/ui-demo.sh start|stop   (after loopback-test.sh created the config and WAV)
DIR=${LTC_TEST_DIR:-$HOME/ltc-mtc-test}
case "$1" in
start)
  ./target/release/ltc-mtc gen "$DIR/loop.wav" --start 00:59:52:00 --fps 25 --seconds 600 --level -18 >/dev/null
  nohup ./target/release/ltc-mtc --config "$DIR/config.json" --listen 0.0.0.0:8091 > "$DIR/demo.log" 2>&1 &
  echo $! > "$DIR/demo.pids"
  sleep 1
  nohup aplay -q -D plughw:CARD=Loopback,DEV=0 "$DIR/loop.wav" >/dev/null 2>&1 &
  echo $! >> "$DIR/demo.pids"
  cat "$DIR/demo.pids";;
stop)
  kill $(cat "$DIR/demo.pids"); rm -f "$DIR/demo.pids";;
esac
