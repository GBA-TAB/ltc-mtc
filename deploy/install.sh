#!/bin/bash
# Builds ltc-mtc on this machine (a Raspberry Pi) and installs it as a systemd service.
# Usage: deploy/install.sh            (run from the source directory; asks for sudo)
#        deploy/install.sh --no-build (install target/release/ltc-mtc as it is)
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "${1:-}" != "--no-build" ]; then
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
sudo install -m 644 deploy/ltc-mtc.service /etc/systemd/system/ltc-mtc.service
sudo modprobe snd-seq 2>/dev/null || true
grep -qx snd-seq /etc/modules 2>/dev/null || echo snd-seq | sudo tee -a /etc/modules >/dev/null
sudo systemctl daemon-reload
sudo systemctl enable ltc-mtc
sudo systemctl restart ltc-mtc
sleep 1
systemctl --no-pager --lines=5 status ltc-mtc || true
ip=$(hostname -I 2>/dev/null | awk '{print $1}')
echo
echo "Open http://${ip:-<this-pi>}:8080/"
