# Arion — Architecture

This document describes the architecture of Arion, a cross-platform
SDR control application for HermesLite 2 / Apache Labs ANAN radios.

## Overview

Arion is structured as a Cargo workspace with 11 library crates and
2 binary crates. The design follows a strict **MVVM** (Model–View–
ViewModel) pattern combined with **Hexagonal Architecture** (Ports &
Adapters), enabling two independent frontends (egui desktop + ratatui
TUI) to share a single application core without code duplication.

```
┌───────────────────────────────────────────────────────────────┐
│                        Frontends (Views)                       │
│  ┌─────────────────┐                   ┌────────────────────┐ │
│  │   arion-egui    │                   │    arion-tui       │ │
│  │   (egui+wgpu)   │                   │   (ratatui+cross-  │ │
│  │   Desktop GUI   │                   │    term) Console   │ │
│  └────────┬────────┘                   └─────────┬──────────┘ │
│           │ reads & dispatches                   │             │
│           └──────────────┬───────────────────────┘             │
│                          ▼                                     │
│  ┌──────────────────────────────────────────────────────────┐ │
│  │                    arion-app (ViewModel)                   │ │
│  │  App struct: state, read API, write API, lifecycle        │ │
│  │  Zero dependency on any UI framework                      │ │
│  └──────┬────────────────┬──────────────────┬───────────────┘ │
│         │                │                  │                  │
│         ▼                ▼                  ▼                  │
│  ┌────────────┐  ┌──────────────┐  ┌──────────────────────┐  │
│  │ arion-core │  │arion-settings│  │   arion-script       │  │
│  │  (Model)   │  │   (TOML)     │  │  (Rhai engine)       │  │
│  │  Radio     │  │  Settings    │  │  ScriptEngine        │  │
│  └──────┬─────┘  └──────────────┘  └──────────────────────┘  │
│         │                                                      │
│    ┌────┴────────────────────────┐                             │
│    │        DSP pipeline         │                             │
│    │  ┌──────┐  ┌──────┐        │                             │
│    │  │ wdsp │→ │wdsp- │        │                             │
│    │  │(safe)│  │ sys  │        │                             │
│    │  └──────┘  │(FFI) │        │                             │
│    │            └──┬───┘        │                             │
│    │               │ C libs     │                             │
│    │  FFTW 3.3.10 + rnnoise     │                             │
│    │  + libspecbleach (vendored) │                             │
│    └────────────────────────────┘                             │
│                                                               │
│    ┌────────────────────────────┐                             │
│    │     Network layer          │                             │
│    │  ┌──────────────┐         │                             │
│    │  │  hpsdr-net   │         │                             │
│    │  │  (Session)   │         │                             │
│    │  └──────┬───────┘         │                             │
│    │         │                  │                             │
│    │  ┌──────┴───────┐         │                             │
│    │  │hpsdr-protocol│         │                             │
│    │  │ (wire types) │         │                             │
│    │  └──────────────┘         │                             │
│    └────────────────────────────┘                             │
│                                                               │
│    ┌────────────────────────────┐                             │
│    │     Audio output           │                             │
│    │  ┌──────────────┐         │                             │
│    │  │ arion-audio  │         │                             │
│    │  │ (cpal+rubato)│         │                             │
│    │  └──────────────┘         │                             │
│    └────────────────────────────┘                             │
└───────────────────────────────────────────────────────────────┘
```

## Design principles

1. **The app is the view-model, not the view.** `arion-app::App`
   owns all mutable state. Frontends are "humble views" — they read
   from `&App` to render and call `App::set_*` to dispatch user
   actions. No application logic lives in the rendering code.

2. **Single source of truth.** Every persisted field (frequency,
   mode, volume, NR, band stack, memories, calibration, display
   settings) lives in `App` and flows to disk via `arion-settings`.
   Frontends never write to disk directly.

3. **One command path.** Whether the user clicks a button in egui,
   presses a key in the TUI, or types a Rhai script — the same
   `App::set_rx_frequency` / `App::jump_to_band` / etc. method is
   called, which sends the same `DspCommand` to the DSP thread.
   There is no parallel API for scripts or for the TUI.

4. **Zero UI dependency in the core.** `arion-app`, `arion-core`,
   `arion-settings`, `arion-script` compile and test without any
   graphics library. `cargo test -p arion-app` runs on a headless
   server. This is enforced by Cargo dependency rules — these
   crates never depend on `egui`, `eframe`, `ratatui`, `wgpu`, or
   `crossterm`.

5. **Vendored C dependencies.** FFTW 3.3.10, rnnoise, and
   libspecbleach are compiled from source by `wdsp-sys/build.rs`.
   No `pkg-config`, no system library install. The build is
   identical on Linux, macOS, Windows, and cross-compile.

## Crate responsibilities

### Layer 0 — Protocol & DSP

| Crate | Role | Key types |
|---|---|---|
| `hpsdr-protocol` | HPSDR Protocol 1 wire format (pure data, no I/O) | `IqSample`, `CommandFrame`, `ControlByte` |
| `wdsp-sys` | Raw `extern "C"` FFI to the vendored WDSP library | `OpenChannel`, `SetRXAMode`, `fexchange0`, `SetRXAGrphEQ10`, etc. |
| `wdsp` | Safe Rust wrapper around `wdsp-sys` | `Channel`, `Mode`, `AgcMode`, wisdom management |

### Layer 1 — Infrastructure

| Crate | Role | Key types |
|---|---|---|
| `hpsdr-net` | UDP discovery + `Session` (rx_loop + tx_loop at 381 Hz) | `Session`, `SessionConfig`, `Consumer<IqSample>` |
| `arion-audio` | cpal audio output + ring buffer + rubato resampling + device enumeration | `AudioOutput`, `AudioSink`, `enumerate_output_devices()` |

### Layer 2 — Application core

| Crate | Role | Key types |
|---|---|---|
| `arion-core` | Radio orchestrator: connects net → WDSP → audio, owns the DSP thread | `Radio`, `RadioConfig`, `DspCommand`, `Telemetry` |
| `arion-settings` | TOML serialization (load/save with atomic write) | `Settings`, `DisplaySettings`, `DspDefaults`, `Calibration`, `Memory` |
| `arion-app` | **View-model (the hexagon center).** Owns `App` struct with all UI state + read/write API + lifecycle | `App`, `RxState`, `Band`, `BandStack`, `FilterPreset`, `AgcPreset`, `WindowKind` |
| `arion-script` | Rhai scripting engine + function bindings | `ScriptEngine`, `ReplLine`, `ReplLineKind` |

### Layer 3 — Frontends (Views)

| Crate | Role | Key types |
|---|---|---|
| `arion-egui` | egui + wgpu desktop frontend | `EguiView`, `Waterfall`, `SpectrumOverlay` |
| `arion-tui` | ratatui + crossterm console frontend | `TuiView`, `TuiWaterfall`, `Focus` |

### Binaries

| Binary | Crate | Entry point |
|---|---|---|
| `arion` | `apps/arion` | `main.rs` → `arion_egui::run()` |
| `arion-tui` | `apps/arion-tui` | `main.rs` → `TuiView::new(opts).run()` |

## Design patterns

### Command

Every mutation flows through a single channel. UI clicks, script
calls, and keyboard shortcuts all produce the same `App::set_*`
method call, which internally sends a `DspCommand` enum variant to
the DSP thread via `mpsc::Sender`. The DSP thread drains the
command queue once per buffer cycle and applies changes atomically.

```
User gesture → App::set_rx_frequency(rx, hz)
                    │
                    ├─ updates App.rxs[rx].frequency_hz
                    ├─ calls Radio.set_rx_frequency(rx, hz)
                    │       └─ sends DspCommand::SetRxFrequency { rx, hz }
                    │               └─ DSP thread: channels[rx].set_passband(...)
                    └─ calls App.mark_dirty() → debounced TOML save
```

### Facade

`App` is the single entry point for all application logic. A
frontend never touches `Radio`, `Settings`, or `ScriptEngine`
directly. It calls `app.connect()`, `app.set_rx_mode()`,
`app.load_memory()`, etc. The `App` struct orchestrates the
underlying subsystems.

### Humble View

Frontends contain minimal logic:

```rust
// egui frontend — read state, render, dispatch
fn draw_rx_row(&mut self, ui: &mut egui::Ui, rx: usize) {
    let state = self.app.rx(rx).cloned().unwrap_or_default(); // read
    let mut freq = state.frequency_hz as f64;
    if ui.add(egui::DragValue::new(&mut freq)).changed() {     // render
        self.app.set_rx_frequency(rx as u8, freq as u32);      // dispatch
    }
}
```

No `mark_dirty`, no `DspCommand`, no `Settings::save` in the view.
The same pattern applies to the TUI frontend — it reads from `App`
and dispatches via `App::set_*`.

### Adapter

Mode conversion between the runtime enum (`wdsp::Mode`) and the
serializable enum (`arion_settings::Mode`) is centralized in
`arion-app` via `mode_to_serde` / `mode_from_serde`. Frontends
and the scripting layer never do this conversion themselves.

### Dependency Inversion

- `arion-core` (Model) does not know about `arion-app` (ViewModel)
- `arion-app` does not know about `arion-egui` or `arion-tui` (Views)
- `arion-script` depends on `arion-app`, not the other way around

This means lower layers are reusable independently. You could write
a third frontend (web, mobile) by depending only on `arion-app`
without touching any other crate.

## Data flow

### RX pipeline (runtime)

```
HL2 radio (UDP 1024)
    │
    ▼
hpsdr-net::Session::rx_loop
    │  parses HPSDR P1 frames → IqSample per RX
    ▼
rtrb::Producer<IqSample> ──── ring buffer ────► rtrb::Consumer
    │                                               │
    │                                               ▼
    │                                    arion-core::dsp_loop
    │                                        │ fexchange0()
    │                                        │ (WDSP DSP chain:
    │                                        │  bandpass → AGC →
    │                                        │  NR → EQ → demod)
    │                                        ▼
    │                                    demodulated audio f32
    │                                        │
    │                                        ▼
    │                                    rtrb ring → arion-audio
    │                                        │       cpal callback
    │                                        ▼
    │                                      speakers
    │
    │  (also from dsp_loop, every ~43 ms)
    ▼
arc_swap::ArcSwap<Telemetry>
    │  spectrum FFT bins, S-meter, center freq, mode
    ▼
Frontend reads telemetry_snapshot() each frame → renders
```

### Telemetry (DSP → UI)

The DSP thread publishes a `Telemetry` struct via `ArcSwap` — a
lock-free atomic pointer swap. The frontend loads the latest
snapshot with `telemetry.load_full()` once per frame. No mutex, no
blocking, no allocation on the hot path.

### Settings persistence

```
User action → App::set_*() → App.mark_dirty()
    │
    ▼
App::tick() called each frame
    │  checks: dirty && elapsed >= 10s (debounce)
    ▼
App::save_now()
    │  App::to_settings() → Settings struct
    │  Settings::save_default()
    │       │  write to arion.toml.tmp
    │       │  rename → arion.toml (atomic)
    ▼
    done (dirty = false, last_save = now)
```

Explicit saves happen on connect, disconnect, and app shutdown
regardless of the debounce timer.

## Threading model

```
Main thread (UI)
    │
    ├─ egui/eframe event loop  OR  ratatui/crossterm event loop
    │     calls App::tick() each frame
    │     reads Telemetry via ArcSwap (lock-free)
    │     sends DspCommand via mpsc::Sender
    │
    ├─ DSP thread (spawned by Radio::start)
    │     SCHED_FIFO priority 80 on Linux (graceful fallback)
    │     drains DspCommand queue
    │     calls wdsp::Channel::fexchange0 per RX
    │     pushes audio to rtrb ring
    │     publishes Telemetry via ArcSwap
    │
    ├─ Network RX thread (spawned by Session)
    │     reads UDP packets from HL2
    │     parses IqSample, pushes to rtrb ring per RX
    │
    ├─ Network TX thread (spawned by Session)
    │     sends C&C heartbeat frames at 381 Hz
    │     carries RX NCO frequency updates
    │
    └─ cpal audio callback thread (managed by cpal)
          reads from rtrb ring → writes to audio device
```

No thread shares mutable state directly. Communication is via:
- `mpsc::channel` for commands (UI → DSP)
- `rtrb` lock-free ring buffers for samples (net → DSP → audio)
- `ArcSwap` for telemetry snapshots (DSP → UI)

## Scripting architecture

The Rhai scripting engine (`arion-script`) binds to `App` — not to
`Radio` or to a specific frontend. This means:

- Scripts work identically in the egui REPL and the TUI REPL
- Scripts use the same `App::set_*` API as the UI
- Scripts are headless-testable (no UI framework needed)

The command flow is indirect for thread safety: scripts push
command strings into a Rhai scope variable (`_cmds`), and the
frontend calls `apply_pending_commands(&mut app)` after eval to
dispatch them through `App`. This avoids storing `&mut App` across
the Rhai eval boundary.

```
User types: freq(0, 14074000)
    │
    ▼
Rhai eval: calls registered fn → pushes "set_freq 0 14074000" to _cmds
    │
    ▼
Frontend: script.apply_pending_commands(&mut app)
    │  parses "set_freq 0 14074000"
    │  calls app.set_rx_frequency(0, 14074000)
    ▼
Same path as a UI click
```

## Cross-platform build

The `wdsp-sys/build.rs` handles all platform differences:

| Target | FFTW malloc | Windows.h | POSIX shim | Link libs |
|---|---|---|---|---|
| Linux native | system `posix_memalign` | shim/Windows.h (POSIX stubs) | shim/wdsp_posix.c | pthread, m |
| Linux → Windows cross | `WITH_OUR_MALLOC` CFLAG | shim-win/Windows.h (case fix) | skipped (w32api native) | avrt, winmm |
| macOS native | system `posix_memalign` | shim/Windows.h (POSIX stubs) | shim/wdsp_posix.c | (framework libs) |

The `cmake` crate detects `CARGO_CFG_TARGET_OS` and injects the
appropriate `CMAKE_SYSTEM_NAME` for FFTW's cmake build. The `cc`
crate picks the right C compiler for the target automatically.
