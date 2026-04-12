# Arion

A modern, cross-platform SDR control application for Apache Labs
(ANAN, Saturn) and HermesLite 2 radios, written in Rust. Inspired by
[Thetis](https://github.com/ramdor/Thetis) (archived April 2026),
Arion is a ground-up rewrite — not a port — targeting Linux, macOS
and Windows from day one.

## Features

- **Multi-RX** (2 independent DDC receivers), NR3 (RNNoise), NR4
  (libspecbleach), ANF, SNBA, binaural audio, 10-band graphic EQ
- **Click-to-tune**, wheel-tune, band buttons, filter presets +
  variable filter, S-meter in S-units with per-band calibration
- **TOML persistence** (settings, band stacks, memories)
- **Rhai scripting** with built-in REPL (syntax highlighting via
  egui_code_editor)
- **Two frontends** sharing the same `arion-app` view-model (MVVM):
  - `arion` — egui + wgpu desktop (DSEG7 7-segment VFO, waterfall,
    spectrum with peak hold + average, resizable panels, floating
    windows, Setup with 5 tabs)
  - `arion-tui` — ratatui console (waterfall HalfBlock, side panel,
    popups, mouse support, works over SSH/tmux on a headless Pi)
- **Fully vendored C dependencies** (FFTW 3.3.10, rnnoise,
  libspecbleach) — no pkg-config, no system libraries required
- **Cross-compile** Linux → Windows (`x86_64-pc-windows-gnu`)
- **Instant first launch** thanks to embedded FFTW wisdom blob

## Workspace layout

```
crates/
  wdsp-sys/         Raw FFI to the vendored WDSP C library
  wdsp/             Safe Rust wrapper (Channel, Mode, EQ, ANF, wisdom)
  hpsdr-protocol/   HPSDR Protocol 1 packet types
  hpsdr-net/        UDP discovery + multi-RX session
  arion-audio/      cpal output + ring buffer + rubato resampling
  arion-core/       Radio orchestrator (net → WDSP → audio)
  arion-settings/   TOML persistence (Settings, Calibration, etc.)
  arion-app/        Headless view-model (MVVM, zero UI dependency)
  arion-script/     Rhai scripting engine + bindings
  arion-egui/       egui desktop frontend
  arion-tui/        ratatui console frontend
apps/
  arion/            Desktop binary (eframe)
  arion-tui/        Console binary (crossterm, headless-friendly)
thetis-upstream/    Git submodule: original Thetis source (read-only reference)
```

## System requirements

- Rust stable ≥ 1.82
- An audio backend supported by [cpal](https://crates.io/crates/cpal):
  ALSA (Linux), CoreAudio (macOS), WASAPI (Windows)
- For the desktop UI: Vulkan, Metal, or DX12 via wgpu

FFTW3, rnnoise, and libspecbleach are **vendored** in
`crates/wdsp-sys/vendor*/` and built by `build.rs` via the `cmake`
and `cc` crates. No external C libraries or `pkg-config` needed.

## Build

```sh
git clone --recurse-submodules <url> arion
cd arion
cargo build --workspace
```

### Cross-compile Linux → Windows

```sh
# 1. Install the cross C compiler (Arch: pacman -S mingw-w64-gcc)
# 2. Add the Rust target
rustup target add x86_64-pc-windows-gnu
# 3. Build. On distros where /usr/bin/rustc shadows rustup (Arch),
#    force PATH so rustup's rustc with the windows-gnu sysroot wins:
PATH="$HOME/.cargo/bin:$PATH" \
  cargo build --target x86_64-pc-windows-gnu --release -p arion
```

The output is `target/x86_64-pc-windows-gnu/release/arion.exe`.
`wdsp-sys/build.rs` detects the target and:
- Builds FFTW 3.3.10 with `WITH_OUR_MALLOC` (mingw lacks
  `posix_memalign` / `memalign`)
- Injects `shim-win/Windows.h` to fix casing (WDSP includes
  `<Windows.h>`, w32api ships `<windows.h>`)
- Skips the POSIX shim (mingw's w32api provides the real Win32 types)
- Links against `avrt` + `winmm`

## Running

```sh
# Desktop (egui)
HL2_IP=192.168.1.40 cargo run -p arion --release

# Console (ratatui — works over SSH/tmux)
HL2_IP=192.168.1.40 cargo run -p arion-tui-bin
```

## License

GPL-2.0-or-later
