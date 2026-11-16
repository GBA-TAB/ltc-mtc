#!/bin/bash
# Builds ltc-mtc on this machine (a Raspberry Pi) and installs it as a systemd service.
# Usage: deploy/install.sh [--port N] [--no-build]
#   run from the source directory; asks for sudo.
#   --port N     web interface port (default 8088)
#   --no-build   install target/release/ltc-mtc as it is
set -euo pipefail
cd "$(dirname "$0")/.."

PORT=8088
BUILD=1
while [ $# -gt 0 ]; do
  case "$1" in
    --port) PORT=$2; shift 2;;
    --no-build) BUILD=0; shift;;
    *) echo "unknown option $1" >&2; exit 1;;
  esac
done
# Another program on the port? (Not ltc-mtc itself, which is replaced below.)
if ss -ltnH "sport = :$PORT" 2>/dev/null | grep -q . && ! systemctl is-active -q ltc-mtc 2>/dev/null; then
  echo "Port $PORT is already in use:" >&2
  ss -ltnH "sport = :$PORT" >&2
  echo "Choose another one with --port N." >&2
  exit 1
fi

if [ $BUILD = 1 ]; then
  if ! command -v cargo >/dev/null; then
    echo "Rust is missing: curl https://sh.rustup.rs -sSf | sh   (then log in again)" >&2
    exit 1
  fi
  if ! pkg-config --exists alsa 2>/dev/null; then
    echo "Installing the ALSA headers (libasound2-dev) and pkg-config"
    sudo apt-get install -y libasound2-dev pkg-config
  fi
  # -j2 keeps a 1 GB Pi 3 out of swap.
  nice cargo build --release -j2
fi

sudo install -m 755 target/release/ltc-mtc /usr/local/bin/ltc-mtc
sed "s/--listen 0.0.0.0:8088/--listen 0.0.0.0:$PORT/" deploy/ltc-mtc.service \
  | sudo tee /etc/systemd/system/ltc-mtc.service >/dev/null
sudo modprobe snd-seq 2>/dev/null || true
grep -qx snd-seq /etc/modules 2>/dev/null || echo snd-seq | sudo tee -a /etc/modules >/dev/null
sudo systemctl daemon-reload
sudo systemctl enable ltc-mtc
sudo systemctl restart ltc-mtc
sleep 1
systemctl --no-pager --lines=5 status ltc-mtc || true
ip=$(hostname -I 2>/dev/null | awk '{print $1}')
echo
echo "Open http://${ip:-<this-pi>}:$PORT/"
