# PLAN.md — Extended Rhai scripting, scripted GUI & rigctld server

Progress tracker for the work detailed in
[`~/.claude/plans/serene-riding-sky.md`](~/.claude/plans/serene-riding-sky.md).
This document is **kept up to date at every step**: what's to do,
what's in progress, what's delivered.

## Objectives

1. Fully cover the `arion-app::App` API from Rhai.
2. Allow GUI creation (windows, buttons, sliders, menus) from
   Rhai scripts.
3. Expose a **rigctld-compatible** server (port 4532) so that
   WSJT-X, fldigi, GPredict, CQRLOG, etc. can drive Arion.
4. Document everything (`docs/SCRIPTING.md`, `docs/RIGCTLD.md`,
   `examples/scripts/`, built-in `help()` in the REPL).

## Guiding design patterns

- **Single source of truth**: `App` is the sole owner of state.
- **Hexagonal**: `arion-script` and `arion-rigctld` = independent
  adapters around `App`.
- **Command pattern** (rigctld): one `RigCommand` struct per command.
- **Registry pattern** (rhai): `ScriptModule` trait, one module per
  domain (radio, rx, memory, window, ui, help).
- **Builder pattern**: fluent API for the scripted GUI.
- **Immutable value objects**: `Widget`, `FnHandle`, `RigReply`.
- **Decoupled renderer**: `Widget` = pure data, `arion-egui`
  translates to egui.
- **Scoped interior mutability**: `Rc<RefCell<*mut App>>` only
  during `run_line` / `invoke_callback`.
- **Explicit errors**: `thiserror` on the library side, `anyhow`
  on the binary side.
- **Per-module isolated tests**.

---

## Phases

### Done — Phases 1 & 2 — Script engine refactor + domain modules

**To do**: refactor `arion-script` into a modular structure,
scoped `Rc<RefCell<App>>`, Rhai types `Radio`/`Rx` with all
get/set properties, free functions, removal of
`apply_pending_commands`.

**Delivered**:
- `crates/arion-script/src/` restructured:
  - `lib.rs` — facade + 13 tests
  - `engine.rs` — App handle lifecycle
  - `ctx.rs` — `ApiCtx` + `with_app` helper
  - `error.rs` — `ScriptError` (thiserror)
  - `ui_tree.rs` — value objects (`Widget`, `ScriptWindow`,
    `FnHandle`, `UiState`) — used in phase 3
  - `help_data.rs` — stub (phase 6)
  - `modules/{mod,radio,rx,memory,window}.rs`
- Full API coverage: `radio.connected/ip/num_rx/active_rx/
  last_error`, methods `rx(i)`/`connect`/`disconnect`/`save`/`tick`;
  `Rx` exposes `.freq .mode .volume .muted .locked .enabled .filter_lo
  .filter_hi .nr3 .nr4 .agc .nb .nb2 .anf .bin .tnf .eq_enabled
  .eq_gains .s_meter .spectrum .center_freq`. Free functions:
  `freq mode volume nr3/nr4/nb/nb2/anf/bin/tnf agc filter
  filter_preset eq eq_band band tune mute lock active_rx num_rx
  connect disconnect memory_save memory_load memory_delete
  window save audio_device`.
- `apply_pending_commands` removed from callers (egui + tui).
- **Design note**: Rhai 1.x rejects `radio.rx(0).freq = v`
  (invalid LHS on a method result). Solution: `rx(i)` is also
  registered as an **indexer** → `radio[0].freq = v` for writes,
  `radio.rx(0)` for reads/arguments. Documented in the docs.
- Tests: 13/13 green. Workspace build + clippy green.

### Done — Phase 3 — Scriptable UI module (descriptors + builder)

**Delivered**:
- `crates/arion-script/src/modules/ui.rs`: full Rhai API
  — `window(id, title, body)`, `vbox(body)`, `hbox(body)`,
  `label`, `button`, `slider`, `checkbox`, `text_edit`, `separator`,
  `on_change(key, fn)`, `menu_item(path, fn)`, `window_show/hide/toggle`.
- `UiState` extended with a `build_stack: Vec<Vec<Widget>>` stack
  and a `callbacks: HashMap<String, FnPtr>` registry (+ a
  `next_cb_id` counter to generate stable ids); widgets reference
  the callback by id through `FnHandle.name`.
- **FnPtr/AST resolution**: container builders (`window`,
  `vbox`, `hbox`) are registered with a first parameter of type
  `NativeCallContext`; the body (an `FnPtr`) is invoked via
  `FnPtr::call_within_context(&ctx, ())`, which re-enters the
  registered functions and populates the frame at the top of the
  stack. For deferred dispatch (clicks, on_change, menu), the
  callback is invoked via `FnPtr::call(&engine, &ast_empty, args)`
  — no AST is required since the code is already compiled into
  the `FnPtr`.
- `ScriptEngine::ui_state()` and `ScriptEngine::dispatch_callback(&h,
  &mut app)` exposed to the frontend.
- 4 additional tests (builder, slider+on_change, show/hide/toggle,
  menu_item) → 17/17 tests green, clippy clean.

### Done — Phase 4 — egui renderer

**Delivered**:
- `crates/arion-egui/src/script_ui.rs` — `render_script_ui(ctx,
  engine, app)` iterates over `ui_state.windows`, opens one
  `egui::Window` per entry, calls `render_widget` recursively for
  each `Widget` (Label, Button, Checkbox, Slider, TextEdit,
  Separator, V/HBox), collects clicks and modified keys, then
  dispatches the callbacks after releasing the borrow on `UiState`.
- Hook in `EguiView::ui()` after the native windows.
- "Scripts" menu added to the menu bar: built from
  `ui_state.menu_items`; dispatch happens outside the menu (borrow
  released) to avoid re-entrancy.
- Workspace build + clippy + tests green.

### Done — Phase 5 — Startup script

**Delivered**:
- `arion_script::startup_script_path()` — `directories` wrapper
  that returns `~/.config/arion/startup.rhai` (or the macOS/Windows
  equivalent). Same pattern as `arion-settings`.
- New `ScriptEngine::run_script(src, &mut app)` method — multi-
  statement eval without REPL echo, errors pushed as a
  `ReplLineKind::Error` line. `push_output` helper so the frontend
  can inject its own messages.
- `EguiView::load_startup_script()` called from `EguiView::new`.
  Silent if the file is absent, `tracing` warning + REPL error
  line if reading/eval fails.
- 3 tests added to `arion-script`: multi-line mutation,
  error-surface-as-repl-line, startup-path-ends-with.

### Done — Phase 6 — Rhai docs + `help()`

**Delivered**:
- `help_data.rs` filled in: `help_topics()` returns a
  `HashMap<&str, &str>` covering 45+ entries (general topics
  `overview`, `radio`, `rx`, `actions`, `modes`, `bands`, `filters`,
  `agc`, `memories`, `ui`, `startup` + one entry per free function
  and widget).
- New module `modules/help.rs` (`HelpModule`): `help()` returns
  the overview + the sorted list of topics; `help("topic")`
  returns the entry or an error message. Since the return value
  is automatically rendered by `run_line` in the REPL output, no
  side channel is needed.
- Registered in `modules/mod.rs::register_builtins`.
- `docs/SCRIPTING.md` created (~430 lines, English) — intro,
  quickstart, `radio` object, action functions,
  modes/bands/filters/AGC, memories, scripted UI, startup,
  examples, full API reference appendix.
- `examples/scripts/01_basics.rhai` … `06_ui_complete.rhai`
  created, all commented.
- "Scripting" section added to the README (~13 lines).
- Coherence test `help_coherence_core_functions_all_documented`
  checks that 45 critical names are present in `help_topics()`.
- 4 help tests added (overview, topic, unknown, coherence).
- Total: **24 tests green** in `arion-script` (versus 17 before
  phase 5). Build + clippy + workspace tests all green.

### Done — Phase 7 — `arion-rigctld` crate

**Delivered**:
- New crate `crates/arion-rigctld/` (no tokio, `std::net` +
  `std::thread` + `std::sync::mpsc`) added to the `members` and
  `workspace.dependencies` of the root `Cargo.toml`.
- Command pattern architecture:
  - `commands/mod.rs` — `RigCommand` trait, `parse` dispatcher.
  - `commands/freq.rs`, `mode.rs`, `ptt.rs`, `vfo.rs`, `level.rs`,
    `split.rs`, `misc.rs`, `unknown.rs` — one struct per verb.
  - `protocol.rs` — `parse_line` (strips `+`), `format_reply`,
    `mode_to_rigctld` / `parse_rigctld_mode`, `DUMP_STATE` constant.
  - `reply.rs` — `RigReply` enum (`Ok`, `Error`, `Value`,
    `KeyValues`, `Raw`).
  - `session.rs` — per-connection TCP loop (read_line → parse →
    send `RigRequest` → wait sync_channel → write wire + `RPRT 0`).
  - `error.rs` — `RigctldError` via thiserror.
  - `lib.rs` — `RigctldHandle::{start,stop,addr}` (acceptor thread
    + list of per-session join handles), `RigRequest`, `drain()` /
    `drain_with_limit()`.
- Concurrency: each session has its own
  `mpsc::sync_channel::<RigReply>(1)` for the reply; a single
  `mpsc::Sender<RigRequest>` is shared across all sessions and
  drained on the UI side once per frame (max 64 msgs/frame).
  Clean shutdown via `Arc<AtomicBool>`: `Drop` on the handle flips
  the flag, joins the acceptor, then all active sessions.
- Modes: full `WdspMode ↔ rigctld` table (LSB/USB/CW/CWR/AM/AMS
  /FM/PKTLSB/PKTUSB/DSB; Spec/Drm fall back to `USB`).
- Commands implemented: `F/f`, `M/m`, `V/v`, `L AF / l AF`,
  `T/t` (stub), `S/s` (stub), `\chk_vfo`, `\dump_state`, `q/\quit`.
  Unknown verbs → `RPRT -11`.
- **18 tests** green: parse/format round-trip, per-command tests
  using an `App::new(AppOptions::default())` instantiated in the
  test, and 3 TCP session integration tests (OS-chosen port)
  covering the freq round-trip, the error on an unknown verb and
  `\dump_state`.

### Done — Phase 8 — rigctld integration in egui

**Delivered**:
- `arion-settings`: new `NetworkSettings` type
  (`rigctld_enabled = false`, `rigctld_port = 4532`), added to
  `Settings` and persisted via TOML (section `[network]`).
- `arion-app`: `network_settings()` / `network_settings_mut()`
  getters, wired into `to_settings()` and creation (`App::new`).
- `arion-egui`:
  - `EguiView` carries `rigctld_tx`, `rigctld_rx`,
    `rigctld_handle: Option<RigctldHandle>`, `rigctld_status`.
  - `EguiView::ui()` calls `arion_rigctld::drain(&mut self.app,
    &self.rigctld_rx)` right after `self.app.tick()`.
  - **Network** tab added to the Setup window: *Enable rigctld
    server* checkbox, port DragValue (disabled while the server
    is running), status line. A toggle starts/stops
    `RigctldHandle` live.
  - Auto-start in `EguiView::new` if the box was checked at the
    last save.
  - `on_exit` stops the server cleanly before the `App` shutdown.
- TUI not touched (rigctld not exposed there, no dep added).

### Done — Phase 9 — rigctld docs

**Delivered**:
- `docs/RIGCTLD.md` (~180 lines, English): rigctld overview,
  enabling it in Arion, WSJT-X / fldigi / GPredict / CQRLOG
  configuration, supported-commands table, extended `+` mode,
  `netcat` + Python examples, troubleshooting section,
  limitations.
- "External control (rigctld)" section added to the README right
  after "Scripting", pointing at `docs/RIGCTLD.md`.
- Manual WSJT-X test: to be done by the user (requires a physical
  HL2 for a real FT8 QSO).

---

## Global verification

At each phase:
- `cargo build --workspace`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`

Legend: Done · Todo · In progress
