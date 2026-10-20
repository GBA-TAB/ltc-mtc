# Changelog

## 0.3 (2026-10-20)

- **OSC commands** in events, mixed freely with MIDI commands. An OSC command has an address
  and typed arguments (int, float, string, bool), and goes to a target chosen from a list.
- **OSC targets**, defined once in the settings: name, host (IP address or name), port and
  protocol. Each shows its address, the number of messages sent, or its error.
- Protocols: **UDP**, **TCP with SLIP framing** (OSC 1.1, e.g. QLab) and **TCP with a length
  prefix** (OSC 1.0).
- Name lookups run in the background and never delay a cue. TCP targets are connected ahead
  of time and reconnected while they are down. A cue more than a second late is dropped.
- Test scripts: `osc-listen.py`, `osc-test.sh`, `osc-reconnect-test.sh`.

## 0.2 (2026-10-20)

- **MIDI outputs per command**: each CC/PC command goes to the default commands output or to
  a port of its own.
- **MTC on its own port** (`LTC-MTC:mtc`), with its own on/off switch and output. Configs
  from 0.1 keep MTC on the output they used.
- Web interface on port **8088** by default (8080 is often taken). `install.sh --port N`
  checks that the port is free.
- Test script: `routing-test.sh`.

## 0.1 (2026-10-20)

- LTC decoder: 24, 25, 29.97 drop-frame and 30 fps, detected or fixed. Reads reverse,
  varispeed and inverted polarity.
- MIDI Time Code: quarter frames, and Full Frame messages on start, locate and in reverse.
- Events at given timecodes with Control Change and Program Change commands. An event fires
  when the timecode crosses its time.
- Web interface, JSON API, systemd service (real-time priority), `install.sh` for the Pi.
- `gen`, `decode` and `devices` commands. Test scripts: `loopback-test.sh`, `ui-demo.sh`.
