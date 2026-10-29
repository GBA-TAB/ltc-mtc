# LTC generator and PTP-timecode slates: a sketch

Status: sketch only (2026-10-29), nothing built. Asked for by the user via intercom-be: an LTC
generator pod in the MXL plant, and slate generators whose timecode comes from PTP.

## One timecode source for the plant

Every generator computes the same timecode from the same clock, so no box jams another.

- **Clock:** TAI from the host's PTP, through mxl-function's `HostClock` (the wire-timescale rule of
  mxl-stack PTP-HARDENING §6: TAI = wire + `taiMinusWireS` from `.clock-status.json`). Never the raw
  PHC: the FS serves UTC numbering today.
- **Mapping (SMPTE ST 2059-1):** timecode = local time of day = TAI − UTC offset (37 s) + the
  configured local offset (time zone, daylight saving).
  - Integer rates (24, 25, 30, 50, 60): frame = floor(t × fps) since the SMPTE/PTP epoch. Every host
    agrees to the frame without talking to the others.
  - 29.97 / 59.94 (drop-frame): the frame count isn't an integer per second, so it counts from the
    last **daily jam** (ST 2059-1's jam time, e.g. 03:00 local), with drop-frame numbering.
- **Where the offset and jam time come from:** ST 2059-2's synchronisation-metadata TLV, if the
  grandmaster sends it (ours most likely doesn't), else pod parameters.
- **Alignment:** an LTC frame starts on the media sample where t × fps is an integer. MXL's grain
  and sample indices are TAI-based, so the edge falls on a known index, sample-accurate.

## The LTC generator function (one pod)

| | |
|---|---|
| Output | an MXL audio flow, 48 kHz, 1 channel (more channels = the same LTC, or one per rate) |
| Signal | SMPTE 12M biphase LTC, ~25 µs rise, level a parameter (default −18 dBFS) |
| 2110 | ST 2110-30 through the existing mxl-2110 bridge, not a native sender |
| Later | ST 2110-40 ANC with ATC (ST 12-2) for video receivers; MTC/OSC already exist in ltc-mtc |
| Params | fps (24, 25, 30, 29.97 DF, 50, 59.94 DF), local offset / time zone, jam time, user bits (date per ST 309, optional), level |
| Status | BCP-008 / IS-12 like the other functions, plus a clock detail: locked, wire timescale, current TC, last jam |

## What exists to reuse

- **ltc-mtc `src/ltc.rs`:**
  - `encode_bits` (80-bit frame, drop-frame flag, user bits);
  - `LtcEncoder` (biphase, rise time, speed and reverse);
  - the decoder for a loop test;
  - the tests.

  The `Tc`/`Fps` and encoder parts would move into a small shared crate (`tc-ptp`) used by the pod
  and the slates.
- **mxl-function (in the MXL fork):**
  - `HostClock`, which reads the wire timescale;
  - the NMOS / IS-12 / BCP-008 / C-CAT skeleton the mixer, players and gateways use.
- **mxl-play (Intercom):** the closest template for a pod that writes one MXL audio flow and
  registers its Sender. The ident players (ident-1..4) are already this shape.
- **The operator's type.json / podOverride pattern:** for the pod type on caspar, the R440 or the rig.
- **mxl-signal-gen:** the slate base, with a video pattern, text overlay, audio ident and an
  NMOS-controllable start/stop.

## Slates with PTP timecode

mxl-signal-gen's "Timecode overlay" today burns in GStreamer's running time, not the plant's
timecode. For a PTP slate:

- compute the timecode per frame from the MXL grain index with the same `tc-ptp` function;
- feed it to the text overlay, together with the source name and the date;
- optionally output the same timecode as LTC on an audio channel of the slate.

Then the picture, the slate's LTC and the LTC pod show the same frame on every host.

## Checks once built

- Decode the pod's own output with the ltc-mtc decoder: the timecode equals PTP-derived TC, with zero
  drift over hours.
- Two hosts (caspar and the rig) agree to the frame. Read the 2110-30 copy on the R440.
- Drop-frame: a jam at the configured time, and the minute-boundary numbering, against reference
  tables.
- Conformance: `mxlctl conformance` at 0 fail, like the other functions.

## Open points

- Does the plant's grandmaster send the ST 2059-2 TLV? If not, the time zone, daylight saving and jam
  time are pod parameters, the same on every pod.
- Which rates and how many LTC flows (one per rate, or one pod per rate).
- Where it runs first (caspar: intercom-e4 deploys), and who owns the pod type.
- Slates: extend mxl-signal-gen (whose repo?) or a new small slate function.
