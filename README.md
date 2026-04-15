<p align="center">
  <strong>Arion</strong> — A modern SDR control application for HermesLite 2 & Apache Labs radios
</p>

<p align="center">
  <img src="https://img.shields.io/badge/status-proof--of--concept-orange" alt="Status: PoC">
  <img src="https://img.shields.io/badge/license-GPL--3.0--or--later-blue" alt="License: GPL-3.0-or-later">
  <img src="https://img.shields.io/badge/rust-stable%20%E2%89%A5%201.82-brightgreen" alt="Rust: stable ≥ 1.82">
  <img src="https://img.shields.io/badge/platform-linux%20%7C%20macOS%20%7C%20windows-lightgrey" alt="Platform: Linux | macOS | Windows">
</p>

---

> **Warning — Proof of Concept.**
> Arion is under active development and not production-ready.
> APIs, file formats, and configuration will change without notice.
> Use at your own risk — contributions and feedback welcome.

## Screenshots

**Desktop** (egui + wgpu)

![Arion Desktop](assets/screenshots/desktop.png)

**Console** (ratatui — works over SSH/tmux)

![Arion TUI](assets/screenshots/tui.png)

## What is Arion?

Arion is a cross-platform SDR (Software Defined Radio) control
application for [HermesLite 2](http://www.hermeslite.com/) and
Apache Labs ANAN radios, written in Rust. It communicates with the
radio hardware via HPSDR Protocol 1 over Ethernet.

Inspired by [Thetis](https://github.com/ramdor/Thetis) (archived
April 2026), Arion is a **ground-up rewrite** — not a port — with
a modern architecture designed for multiple platforms and frontends
from day one.

### Why Arion?

In Greek mythology, Arion was a legendary musician whose song was so
powerful that a dolphin carried him safely across the sea. Like its
namesake, Arion transforms waves into music — radio waves into audio.

## Features

- **Dual RX** — two independent DDC receivers (RX1 + RX2)
- **DSP** — four independent noise reducers (NR/ANR, NR2/EMNR,
  NR3/RNNoise, NR4/libspecbleach), ANF auto-notch, SNBA + BPSNBA
  spectral blankers, NB (ANB) + NB2 (NOB) time-domain blankers,
  TNF tracking-notch filter (multiple notches per RX), squelch
  (FM / AM / SSB auto-routing), APF (audio peak filter for CW),
  binaural mixing, fine-grained AGC (top / hang / decay / fixed
  gain), FM CTCSS + deviation, 10-band graphic EQ, variable
  passband filter
- **Digital modes** — PSK31, PSK63, RTTY (Baudot ITA2), APRS
  (AFSK Bell 202 + HDLC + AX.25 UI frames), FT8 (via vendored
  `ft8_lib`), and WSPR (Fano K=32 r=1/2 decoder from WSJT-X, Rust
  demod + 375 Hz baseband resampler). Full round-trip tested
  encoders + demodulators, UTC-aligned slots (15 s for FT8, 120 s
  for WSPR), Ctrl+click signal browser on the spectrum,
  constellation diagram for PSK-family modes. See
  [`docs/DIGITAL-MODES.md`](docs/DIGITAL-MODES.md) for the user
  guide and the `liquid` / `ft8` / `wsprd` crates below.
- **Spectrum & Waterfall** — real-time display with peak hold,
  averaging, configurable dB range, spectrum fill
- **S-Meter** — S-units display with per-band calibration
- **Band stack** — quick-jump between amateur bands with memory
- **Memories** — named frequency/mode bookmarks
- **External control** — four surfaces in parallel:
  - **rigctld** (Hamlib subset, TCP 4532) — WSJT-X, fldigi,
    GPredict, CQRLOG compatibility
  - **MIDI** (crate `arion-midi`) — hot-swappable CC / Note
    mapping, learn mode, presets for X-Touch Mini + BeatStep
  - **REST API** (crate `arion-api`, `/api/v1/*`) — JSON over
    HTTP, Prometheus `/metrics`, OpenAPI 3.1 spec, optional
    Rhai `/scripts/eval` endpoint
  - **arion-web** — WebSocket bridge to a browser frontend
- **Rhai scripting** — built-in REPL with syntax highlighting;
  every UI action is scriptable
- **Two frontends** on one shared core (MVVM architecture):
  - `arion` — egui + wgpu desktop with 7-segment VFO display,
    resizable panels, floating windows, Setup with 7 tabs
  - `arion-tui` — ratatui console for SSH / tmux / headless servers
- **Self-contained build** — FFTW, rnnoise, libspecbleach vendored;
  no system libraries or `pkg-config` needed
- **Cross-compile** — Linux → Windows in one command
- **Instant startup** — embedded FFTW wisdom blob (first launch <1s)

## Quick start

### Prerequisites

- Rust stable ≥ 1.82
- A [HermesLite 2](http://www.hermeslite.com/) or Apache Labs ANAN
  radio on the local network
- Audio: ALSA (Linux), CoreAudio (macOS), WASAPI (Windows)
- GPU: Vulkan, Metal, or DX12 (for the desktop frontend only)

### Build & run

```sh
git clone --recurse-submodules <url>
cd arion

# Desktop (egui)
HL2_IP=192.168.1.40 cargo run -p arion --release

# Console (ratatui — no GPU required, works over SSH)
HL2_IP=192.168.1.40 cargo run -p arion-tui-bin
```

### Cross-compile Linux → Windows

```sh
# Install cross compiler (Arch: pacman -S mingw-w64-gcc)
rustup target add x86_64-pc-windows-gnu
PATH="$HOME/.cargo/bin:$PATH" \
  cargo build --target x86_64-pc-windows-gnu --release -p arion
# → target/x86_64-pc-windows-gnu/release/arion.exe
```

## Architecture

Arion follows a strict **MVVM + Hexagonal Architecture** pattern:

```
Frontends (Views)          arion-egui (desktop)
                           arion-tui  (console)
                                │
ViewModel                  arion-app  (headless, zero UI dep)
                                │
Model / Ports              arion-core (Radio, DSP thread)
                           arion-settings (TOML persistence)
                           arion-script (Rhai engine)
                                │
Infrastructure             wdsp / wdsp-sys (WDSP C FFI)
                           liquid / liquid-sys (digital DSP FFI)
                           ft8 / ft8-sys (ft8_lib FFI)
                           hpsdr-net (HPSDR P1 UDP)
                           arion-audio (cpal + rubato)
```

Key design rules:
- **One command path** — UI clicks, keyboard shortcuts, and Rhai
  scripts all call the same `App::set_*` methods
- **Humble views** — frontends only read state and dispatch actions
- **Zero UI dep in core** — `arion-app` compiles and tests headless

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for full details
including data flow diagrams, threading model, and design patterns.

## Workspace

```
crates/
  wdsp-sys/          Raw FFI to vendored WDSP (FFTW, rnnoise, specbleach)
  wdsp/              Safe Rust wrapper (Channel, Mode, EQ, wisdom)
  liquid-sys/        Raw FFI to vendored liquid-dsp (modems, NCO, symsync)
  liquid/            Safe Rust wrapper (Modem, MsResamp, Nco, SymSync)
  ft8-sys/           Raw FFI to vendored ft8_lib (KGoba) — Monitor + LDPC
  ft8/               Safe Rust wrapper (encode_to_audio + Monitor::decode)
  wsprd-sys/         Raw FFI to the FFTW-free subset of WSJT-X's wsprd (Fano, unpk)
  wsprd/             Safe Rust wrapper around wsprd-sys (channel_symbols, fano_decode, unpack)
  hpsdr-protocol/    HPSDR Protocol 1 wire types
  hpsdr-net/         UDP discovery + multi-RX session
  arion-audio/       cpal output + ring buffer + rubato resampling
  arion-core/        Radio orchestrator (net → DSP → audio)
  arion-settings/    TOML persistence (atomic write)
  arion-app/         Headless view-model (MVVM core) + protocol DTOs
  arion-script/      Rhai scripting engine + bindings
  arion-rigctld/     Hamlib rigctld-compatible TCP server
  arion-midi/        MIDI controller bridge (midir + wmidi)
  arion-api/         REST / JSON HTTP API (axum) + Prometheus metrics
  arion-web/         Browser frontend + WebSocket + WebRTC audio bridge
  arion-egui/        egui desktop frontend (DSEG7 VFO, waterfall, EQ, REPL)
  arion-tui/         ratatui console frontend (waterfall, side panel, popups)
apps/
  arion/             Desktop binary (egui)
  arion-tui/         Console binary (ratatui)
  arion-web/         Headless web-only binary
thetis-upstream/     Git submodule: original Thetis C# source (read-only reference)
```

## Roadmap

| Phase | Status | Description |
|---|---|---|
| A | Done | Foundations + minimal RX on HermesLite 2 |
| B | Done | Daily-usable RX (multi-RX, NR, click-to-tune, bands, persistence, cross-compile) |
| D | Done | Thetis-style UI, MVVM refactor, Rhai scripting, TUI frontend |
| E | Done (software) | DSP bindings: ANF, SNBA, EQ, 4 NR variants, NB/NB2, TNF, squelch, APF, AGC fine, FM CTCSS/deviation, SAM sub-mode, BPSNBA tuning. E.4 PureSignal / E.5 Diversity / E.6 ALEX routing still blocked on antenna |
| C | Partial | rigctld ✅, MIDI ✅, REST API ✅; TX + Kenwood CAT + TCI still pending |
| F | Planned | CI, installers, audio recording, scheduler |

## Known limitations

- **No TX** — transmit pipeline not implemented yet (Phase C)
- **No TCI server** — SkimmerServer / JTDX TCI mode won't connect.
  Design exists in `todo/tci.md`
- **No Kenwood CAT** — only the Hamlib subset via rigctld is
  available; Thetis' native TS-2000 + `ZZ*` command set is not
  implemented
- **Single sample rate** — 48 kHz only from the radio; rubato 2.0
  handles device-side resampling
- **No installer** — build from source required (Phase F)
- **No authentication on external surfaces** — rigctld and the REST
  API bind to loopback by default. Exposing them on a LAN requires
  a reverse proxy with auth

## Scripting

Arion embeds a [Rhai](https://rhai.rs) scripting engine. The desktop
app ships a REPL + multi-tab editor (menu *View → Scripts*), lets you
build custom panels and menus, and auto-loads a startup script.

- **Full reference** — see [`docs/SCRIPTING.md`](docs/SCRIPTING.md).
- **Examples** — `examples/scripts/01_basics.rhai` …
  `06_ui_complete.rhai`.
- **Startup script** — `~/.config/arion/startup.rhai` (Linux) is
  loaded once on launch; use it to declare persistent windows, menu
  items, and presets.
- **REPL help** — type `help()` or `help("topic")`.

## External control

Arion exposes four independent control surfaces, all toggleable
from *Setup → Network* or via the persisted `arion.toml`:

### rigctld (Hamlib subset, TCP 4532)

Tested against WSJT-X, fldigi, GPredict, CQRLOG.

- **Full reference** — [`docs/RIGCTLD.md`](docs/RIGCTLD.md)
- **WSJT-X** — *Radio = Hamlib NET rigctl*, *Network Server =
  `127.0.0.1:4532`*, *Poll Interval = 1 s*

### REST / JSON HTTP API (default port 8081)

Resource-oriented JSON API under `/api/v1/*`, documented in
[`docs/API.md`](docs/API.md) and [`docs/openapi.yaml`](docs/openapi.yaml).
Includes a Prometheus text-format `/metrics` endpoint and an
optional gated `/scripts/eval` for Rhai.

```sh
curl -s http://127.0.0.1:8081/api/v1/instance | jq
curl -X PATCH http://127.0.0.1:8081/api/v1/rx/0 \
     -H 'content-type: application/json' \
     -d '{"frequency_hz": 14074000, "mode": "USB"}'
curl -X POST http://127.0.0.1:8081/api/v1/bands/M20
```

### MIDI controllers

Map any USB MIDI controller (knobs, pads, encoders) to Arion
actions — VFO tuning, mode change, band jump, NR toggle, memory
recall, …

- **Full reference** — [`docs/MIDI.md`](docs/MIDI.md)
- **Presets** — [`docs/midi-presets/beatstep.toml`](docs/midi-presets/beatstep.toml),
  [`docs/midi-presets/x-touch-mini.toml`](docs/midi-presets/x-touch-mini.toml)
- **Setup UI** — *Setup → MIDI* with Learn mode (shows the last
  received event) and hot-swap binding edits (no listener restart)
- **Persistence** — `~/.config/arion/midi.toml`

### arion-web (WebSocket + WebRTC audio — prototype)

Browser frontend with live state push and audio streaming.
Enabled via `ARION_WEB_LISTEN=<addr>`. Design may evolve.

## Credits

Arion stands on the shoulders of a lot of amateur-radio, DSP and
systems work. The binary wouldn't exist without the people and
projects below, who each get the credit and the blame-for-
misuse-by-us separately:

- **[Thetis](https://github.com/ramdor/Thetis)** — Rich (W4WMT), Doug (W5WC),
  Warren (NR0V), and the whole PowerSDR/Thetis lineage. Arion is a
  ground-up rewrite, not a port, but the feature set, terminology,
  and a lot of the DSP conventions come straight from Thetis.
- **[WDSP](https://github.com/TAPR/OpenHPSDR-Thetis/tree/master/Project%20Files/Source/wdsp)** —
  Warren Pratt (NR0V). The C DSP core behind every RX in Arion
  (demod, NR family, ANF, AGC, EQ, waterfall). Vendored under
  `crates/wdsp-sys/vendor/`.
- **[FFTW](https://www.fftw.org/)** — Matteo Frigo and Steven G. Johnson (MIT).
  Powers every FFT inside WDSP. Vendored (single + double precision
  build) alongside WDSP.
- **[RNNoise](https://github.com/xiph/rnnoise)** — Jean-Marc Valin /
  Xiph.Org. Feeds the NR3 noise reducer.
- **[libspecbleach](https://github.com/lucianodato/libspecbleach)** —
  Luciano Dato. Drives the NR4 spectral noise reducer.
- **[liquid-dsp](https://liquidsdr.org/)** — Joseph Gaeddert, Virginia
  Tech. Modems, symbol sync, NCO, polyphase resampler used by
  the PSK31 / RTTY / APRS pipelines. Vendored under
  `crates/liquid-sys/vendor/`.
- **[ft8_lib](https://github.com/kgoba/ft8_lib)** — Kārlis Goba (YL3JG).
  Waterfall + Costas sync + LDPC for FT8 decoding. Vendored under
  `crates/ft8-sys/vendor/` (also includes Mark Borgerding's
  [kissfft](https://github.com/mborgerding/kissfft)).
- **[WSJT-X](https://wsjt.sourceforge.io/)** — Joe Taylor (K1JT),
  Steven Franke (K9AN), and contributors. Arion's WSPR decoder
  compiles the FFTW-free subset of WSJT-X's `lib/wsprd/` (Fano
  decoder, metric tables, callsign hash, unpacker, encoder
  utilities). The K=32 rate-1/2 Fano sequential decoder itself is
  **Phil Karn's (KA9Q)** original work, minor modifications by
  K1JT. Vendored under `crates/wsprd-sys/vendor/`.
- **Rust crates** that do a lot of heavy lifting:
  [egui](https://github.com/emilk/egui) (Emil Ernerfeldt),
  [wgpu](https://wgpu.rs/),
  [ratatui](https://github.com/ratatui/ratatui),
  [cpal](https://github.com/RustAudio/cpal) and
  [rubato](https://github.com/HEnquist/rubato) (Henrik Enquist)
  for audio I/O and resampling,
  [rustfft](https://github.com/ejmahler/RustFFT),
  [rhai](https://github.com/rhaiscript/rhai),
  [axum](https://github.com/tokio-rs/axum),
  [midir](https://github.com/Boddlnagg/midir) + [wmidi](https://github.com/RustyYato/wmidi),
  [arc-swap](https://github.com/vorner/arc-swap),
  [rtrb](https://github.com/mgeier/rtrb),
  and many more listed in `Cargo.lock`.

Vendored C sources keep their upstream licenses (see each
`vendor/` directory's `LICENSE` / `COPYING`). Arion as a whole is
distributed under GPL-3.0-or-later, which is compatible with all
of the above (the strongest constraint being WSJT-X's GPLv3).

## Contributing

Arion is in early development. Bug reports and feature requests via
[issues](../../issues) are welcome. If you'd like to contribute code,
please open an issue first to discuss the approach.

## License

[GPL-3.0-or-later](LICENSE)
