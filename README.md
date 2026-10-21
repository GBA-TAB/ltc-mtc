# ltc-mtc

An SMPTE LTC reader for the Raspberry Pi (3 and later) and other Linux machines:

- **reads LTC** from an ALSA audio input: 24, 25, 29.97 drop-frame and 30 fps, detected
  automatically or fixed. It also reads LTC played in reverse, at varispeed or with inverted
  polarity;
- **sends MIDI Time Code** to the output you choose, or none: quarter frames while the
  timecode runs, and a Full Frame message when it starts, after a locate, and in reverse;
- **fires MIDI events at given timecodes**. Each event is a time (HH:MM:SS:FF) with a list of
  Control Change (channel, controller, value) and Program Change (channel, program) commands.
  Each command goes to the default commands output or to an output of its own, so one event
  can drive several devices;
- **sends OSC messages at given timecodes**, in the same events as the MIDI commands: an OSC
  address with typed arguments, sent to a target from a list (QLab, a console, ...) over UDP
  or TCP;
- keeps **shows**: named snapshots of the whole device state (settings, routing, cues) with
  **Load**, **Save** and **Save as**. Show files can be downloaded and uploaded;
- has a **start/stop switch** for the cues. MTC keeps running while the cues are stopped;
- has a **web interface** to choose the audio input and channel, the MIDI outputs (ALSA
  sequencer ports), the OSC targets, the frame rate, and to edit, test, import and export
  the cues.

![screenshot](docs/screenshot.png)

## Hardware

The Pi has no audio input, so you need a USB audio interface (any class-compliant one) with
LTC on a line input. Levels from about -50 dBFS to full scale work. For MIDI out, use USB
MIDI interfaces (as many as needed), or any sequencer client on the Pi.

ltc-mtc is the sequencer client **LTC-MTC** with two ports, which `aconnect` and other
programs can also connect to: **LTC-MTC:out** carries the commands that use the default
output, and **LTC-MTC:mtc** carries the MIDI Time Code.

## Install on the Pi

Raspberry Pi OS (32 or 64 bit), Bookworm or later:

```sh
curl https://sh.rustup.rs -sSf | sh     # once; log in again afterwards
git clone <this repo> ltc-mtc && cd ltc-mtc
deploy/install.sh
```

`install.sh` installs `libasound2-dev` if needed, builds (a few minutes on a Pi 3), installs
`/usr/local/bin/ltc-mtc` and the systemd service `ltc-mtc`, and starts it. Then open
`http://<pi>:8088/`. If another program uses port 8088, choose another one with
`deploy/install.sh --port N` (the script checks that the port is free).

The service runs with real-time priority for the audio and MIDI threads, under a dynamic user
in the `audio` group. Its files are in `/var/lib/ltc-mtc/`:

| File | Holds |
|---|---|
| `config.json` | the active state: settings, routing, OSC targets, cues |
| `shows/<name>.json` | the saved shows, in the same format |
| `state.json` | the current show and whether the cues are active |

Each file is written through a temporary file and a rename, so a power cut never leaves half
a file.

To show the interface on a screen attached to the Pi, run
`chromium-browser --kiosk http://localhost:8088/`.

## Using it

1. **Audio input & timecode**: choose the audio input (prefer the card's `plughw:` entry;
   `dsnoop:` shares the input with other programs) and the channel carrying LTC.
2. **MIDI outputs**: choose the default output for the commands. Switch MIDI Time Code on or
   off and choose its output, which may be another port. Press **Save**. The level meter
   and the `LTC locked` badge show that the signal arrives, and the badges show where the
   commands and the MTC go.
3. **OSC targets** (if you use OSC): add each destination once with **+ Target**: a name,
   the host (IP address or name), the port and the protocol. Each target shows its address and
   the number of messages sent, or what is wrong.
4. **Events**: use **+ Event**, or **+ At current TC** while the timecode runs. Set the time
   and add commands with **+ CC**, **+ PC** and **+ OSC**. A MIDI command's **→** menu picks
   its output: *Default output* or a specific port. An OSC command picks its target, has an
   address (`/cue/12/go`) and any number of arguments (**+ arg**). **▶ Test** sends an
   event's commands immediately. **Save** (or Ctrl+S) stores the list and makes it active.
5. **Fired** lists what was sent, with the wall clock time and the timecode.

### Shows

A show is the whole device state: the audio input, the MIDI and MTC routing, the OSC
targets and all the cues. The header shows the current show.

- **Save** (Ctrl+S) makes your edits active and writes them to the current show. If no show
  is loaded yet, it works like **Save as…**.
- **Save as…** asks for a name. It does not replace an existing show without asking.
- **Load…** lists the shows on the device, with their number of cues and the date they were
  saved. **Load** makes a show active (unsaved edits are discarded after a question). **⇩**
  downloads the show file and **✕** deletes it. **Upload show file…** stores a downloaded
  show on this or another device.
- **Revert** discards the edits since the last Save.

Show names may use letters (any language), digits, spaces and `- _ . ( ) + &`, up to 64
characters.

### Starting and stopping the cues

The button next to the title switches between **● CUES ACTIVE** and **■ CUES STOPPED**.
The change is immediate; it needs no Save. While the cues are stopped:

- no cue fires, and the timecode display is struck through;
- MTC keeps running, and **▶ Test** still sends;
- after a restart, the cues are still stopped.

When you start again, cues that passed while stopped do not fire afterwards. Firing goes on
from the current position.

### Cue sets

**Export cues** downloads the cues together with the OSC targets they use. **Import cues**
reads such a file (or a show file, or an older export) and replaces or adds to the cues. OSC
targets the cues need are added when the show does not have them. A target the show already
has keeps its own host and port, so a cue set moves between venues without changing their
addresses. The import is not saved until you press **Save**.

Outputs are stored by port name ("Client:Port"), so a USB MIDI interface is found again
after it is unplugged, plugged in elsewhere or renumbered. Missing outputs are shown in red at
the top of the page.

Channels are 1 to 16. Controller, value and program numbers are 0 to 127. Many devices show
programs as 1 to 128, so program 0 there is "1".

### OSC

| Protocol | Framing | For |
|---|---|---|
| UDP | one message per datagram | most devices and consoles |
| TCP (SLIP, OSC 1.1) | SLIP, an END byte on both sides | QLab and other OSC 1.1 software |
| TCP (length, OSC 1.0) | int32 length before each message | OSC 1.0 TCP receivers |

Arguments are `int` (32 bit), `float` (32 bit), `string` and `bool` (sent as the types `T`
and `F`). Every message is sent on its own; there are no bundles. Addresses start with `/`
and have no spaces or pattern characters (`* ? , # [ ] { }`).

Host names are looked up in the background at start, after every change and every 30
seconds, so a slow or failing lookup never delays a cue. A TCP target is connected ahead of
the first cue and reconnected every 2 seconds while it is down. If it restarts, the next cue
opens a new connection. A cue that could not be sent within a second is dropped rather than
sent late. Replies from the target are read and ignored.

### When events fire

- Events fire when the timecode **crosses** their time going forward, on the exact frame.
  Frames the reader missed (a dropout of up to *Gap tolerance* frames, 4 by default) do not
  lose an event.
- Stopping and continuing does not repeat events. Rolling back over an event (in reverse, or
  with a locate) and playing forward over it again fires it again.
- After a **locate** or a start, events in the last *Gap tolerance* frames before the first
  frame read also fire. This way a cue at the very start of a roll is not lost while the
  reader locks.
- Nothing fires in reverse.

### MTC

The MTC follows the LTC with about one audio period of delay (5 ms). The frame clock is
smoothed against the jitter of the audio blocks. A missed LTC frame, a jump or a start sends a
Full Frame message and restarts the quarter-frame cycle. In reverse, a Full Frame message is
sent for every frame. When the LTC stops, the quarter frames stop.

## Command line

```
ltc-mtc [--config FILE] [--listen ADDR:PORT]     run (default 0.0.0.0:8088, ~/.config/ltc-mtc/config.json)
ltc-mtc devices                                  list audio inputs and MIDI outputs
ltc-mtc gen FILE.wav [--start TC] [--fps 25] [--seconds 30] [--rate 48000] [--level -10]
                                                 write an LTC test signal
ltc-mtc decode FILE.wav                          print the timecode in a WAV file
```

The web page uses a small JSON API, which other tools can use too:

| Request | |
|---|---|
| `GET /api/status` | timecode, lock, frame rate, speed, level, MIDI and MTC ports, next event, fired log |
| `GET /api/devices` | audio inputs and MIDI outputs |
| `GET /api/config`, `PUT /api/config` | settings and events (validated and saved) |
| `POST /api/fire` | `{"id": "e3"}` fires a saved event; `{"commands": [...]}` sends commands |
| `POST /api/armed` | `{"armed": true\|false}` starts or stops the cues |
| `GET /api/shows` | the current show and the list of shows |
| `POST /api/shows/save` | `{"name"?, "overwrite"?}` saves the active state (as the current show without a name) |
| `POST /api/shows/load` | `{"name"}` makes a show active |
| `POST /api/shows/get`, `/upload`, `/delete` | download, store (`{"name", "config", "overwrite"?}`) and delete show files |

An event in the config:

```json
{ "id": "e2", "name": "cue 2", "time": "00:59:58:12", "enabled": true,
  "commands": [ { "type": "cc", "channel": 2, "controller": 7, "value": 100 },
                { "type": "pc", "channel": 2, "program": 3, "output": "UM-ONE:UM-ONE MIDI 1" } ] }
```

A command without `output` uses `midi_output`, the default commands output. MTC uses
`mtc_enabled` and `mtc_output`. OSC targets and an OSC command:

```json
"osc_targets": [ { "name": "QLab", "host": "192.168.1.20", "port": 53000, "protocol": "tcp" } ],
...
{ "type": "osc", "target": "QLab", "address": "/cue/12/go",
  "args": [ { "type": "float", "value": 0.75 }, { "type": "string", "value": "on" } ] }
```

## Testing without hardware

`scripts/loopback-test.sh` loads `snd-aloop`, plays a generated LTC file into the loopback,
runs ltc-mtc on it with Midi Through as the output, and shows what `aseqdump` received.
`scripts/routing-test.sh` checks the routing: default commands, a command with its own
output, and MTC on its own port, with MTC on and off. `scripts/osc-test.sh [BINARY udp|tcp|tcp-len]`
sends OSC to `scripts/osc-listen.py`. It covers all argument types, IP and named targets, a
name that does not resolve, and the timing. `scripts/osc-reconnect-test.sh` stops and restarts
a TCP target. `scripts/shows-test.sh` covers shows and the start/stop switch through the API:
save as, save, load, names, overwrite protection, upload, delete, stopped cues, and a
restart.
`scripts/ui-demo.sh start|stop` keeps LTC running for a look at the web interface on port
8088 (`LTC_DEMO_PORT` to change it).

## Status

Version 0.4 (2026-10-21; changes in [CHANGELOG.md](CHANGELOG.md)). Tested on a Raspberry Pi 5 (Raspberry Pi OS trixie, arm64) with
LTC played through `snd-aloop` at 24, 25, 29.97 drop-frame and 30 fps. The tests cover
decoding, MTC (piece order, continuity, rate code), events on the exact frame, routing to
separate outputs, MTC on and off, OSC over UDP and both TCP framings (timing within a few
milliseconds, a TCP target restarting), shows and the start/stop switch (through the API and by clicking
through the page in a browser), the systemd sandbox (real-time priority, saved settings),
and the web interface.

Not yet tested: a real Pi 3, LTC from a real source through a USB audio interface, USB MIDI
interfaces, and real OSC devices (QLab, consoles).

## Building

```sh
sudo apt install libasound2-dev pkg-config
cargo build --release
cargo test
```

Building on the Pi itself is the simplest way to get a binary that matches its OS (armhf or
arm64). The program uses about 0.3 % of a Pi 5 core and 4 MB of memory.
