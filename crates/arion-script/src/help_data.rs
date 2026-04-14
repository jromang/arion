//! Static help content for the REPL `help()` function.
//!
//! Each entry is a short, self-contained snippet kept roughly in sync
//! with `docs/SCRIPTING.md`. Cross-checked by a coherence test in
//! `lib.rs` that asserts every core function name has an entry.

use std::collections::HashMap;

/// All help topics. Key = topic or function name. Value = formatted
/// multi-line string (already `\n`-separated).
pub fn help_topics() -> HashMap<&'static str, &'static str> {
    let mut m: HashMap<&'static str, &'static str> = HashMap::new();

    m.insert("overview",
"Arion — embedded Rhai scripting.

Type help(\"topic\") for more details, for example:
  help(\"radio\")    — root object and RX
  help(\"rx\")       — receiver properties
  help(\"actions\")  — free functions (freq, mode, volume…)
  help(\"modes\")    — accepted mode strings
  help(\"bands\")    — band strings
  help(\"filters\")  — filter presets
  help(\"agc\")      — AGC presets
  help(\"memories\") — memory management
  help(\"ui\")       — scripted windows/buttons/sliders/menus
  help(\"startup\")  — startup script

Full reference: docs/SCRIPTING.md");

    m.insert("radio",
"radio — root object injected into the scope.

Properties (read):
  radio.connected   — bool
  radio.ip          — String
  radio.num_rx      — int
  radio.active_rx   — int
  radio.last_error  — String

Methods:
  radio.rx(i)       — read accessor (function arguments)
  radio[i]          — write accessor (radio[0].freq = v)
  radio.connect()
  radio.disconnect()
  radio.save()
  radio.tick()

The dual rx()/indexer access is a Rhai 1.x constraint: the LHS of
an assignment does not accept a method result, but does accept an
indexer.");

    m.insert("rx",
"Rx — individual receiver. Obtained via radio.rx(i) or radio[i].

Properties (read + write):
  .freq         Hz (int)
  .mode         String (USB, LSB, CW, FM, AM, …)
  .volume       0.0..1.0
  .muted        bool
  .locked       bool
  .enabled      bool
  .filter_lo    Hz (float)
  .filter_hi    Hz (float)
  .nr3 .nr4     bool (RNNoise / specbleach)
  .agc          String (Off, Long, Slow, Med, Fast)
  .nb .nb2      bool
  .anf          bool
  .bin          bool (binaural)
  .tnf          bool
  .eq_enabled   bool
  .eq_gains     array of 11 integers

Read-only:
  .s_meter      dB (float)
  .spectrum     array of float
  .center_freq  Hz");

    m.insert("actions",
"Free functions (REPL ergonomics):

Frequency/mode/volume:
  freq(rx, hz)          mode(rx, s)          volume(rx, v)
  mute(rx, b)           lock(rx, b)

Filters:
  filter(rx, lo, hi)    filter_preset(rx, s)

Noise reduction / AGC:
  nr3(rx, b)   nr4(rx, b)   nb(rx, b)   nb2(rx, b)
  anf(rx, b)   bin(rx, b)   tnf(rx, b)
  agc(rx, preset)

EQ:
  eq(rx, array11)       eq_band(rx, i, gain)

Bands / VFO / RX:
  band(s)               tune(hz)
  active_rx(i)          num_rx(n)

Memories:
  memory_save(name)     memory_load(i)       memory_delete(i)

Windows:
  window(kind, bool)    window_show/hide/toggle(id)

Misc:
  connect()             disconnect()
  save()                audio_device(name)
  help()                help(\"topic\")");

    m.insert("modes",
"Accepted modes (case-insensitive):
  LSB, USB, DSB, CWL, CWU (alias CW),
  FM, AM, DIGU, DIGL, SAM, SPEC, DRM.");

    m.insert("bands",
"Accepted bands (strings):
  \"160\", \"80\", \"60\", \"40\", \"30\", \"20\",
  \"17\", \"15\", \"12\", \"10\", \"6\".

Example: band(\"40\");");

    m.insert("filters",
"Filter presets:
  \"6.0K\" (alias \"6K\"), \"4.0K\" (\"4K\"),
  \"2.7K\", \"2.4K\", \"1.8K\",
  \"1.0K\" (\"1K\"), \"600\", \"400\", \"250\", \"100\".

Example: filter_preset(radio.rx(0), \"2.4K\");");

    m.insert("agc",
"AGC presets:
  \"Off\", \"Long\", \"Slow\", \"Med\" (alias \"Medium\"), \"Fast\".

Example: radio[0].agc = \"Fast\";");

    m.insert("memories",
"Memories:
  memory_save(name)     save active RX → named memory
  memory_load(i)        load memory i into the active RX
  memory_delete(i)      remove memory i

The band stack automatically remembers the current frequency per
band (FIFO stacks). band(\"40\") loads the last entry stored for
40 m.");

    m.insert("ui",
"Scripted UI (descriptors evaluated on the egui side):

Windows & containers:
  window(id, title, || { … })
  vbox(|| { … })    hbox(|| { … })

Widgets:
  label(text)
  button(label, || { … })
  slider(label, key, min, max)
  checkbox(label, key)
  text_edit(key)
  separator()

Callbacks:
  on_change(key, |v| { … })       reacts to a slider/checkbox/text
  menu_item(\"Path/Item\", || { … })   entry in the \"Scripts\" menu

Window control:
  window_show(id)  window_hide(id)  window_toggle(id)

Full example in examples/scripts/03_custom_panel.rhai.");

    m.insert("startup",
"Startup script:
  ~/.config/arion/startup.rhai   (Linux)
  ~/Library/Application Support/arion/startup.rhai  (macOS)
  %APPDATA%\\arion\\startup.rhai  (Windows)

Loaded and executed once when egui launches.
Ideal for declaring persistent windows, menus, presets.");

    // Per-function entries (signature + example).
    m.insert("freq",
"freq(rx, hz) — sets the RX frequency.
  freq(radio.rx(0), 14074000);
  // equivalent: radio[0].freq = 14074000;");
    m.insert("mode",
"mode(rx, s) — changes the demodulation mode.
  mode(radio.rx(0), \"USB\");
  // equivalent: radio[0].mode = \"USB\";");
    m.insert("volume",
"volume(rx, v) — sets the volume (0.0..1.0).
  volume(radio.rx(0), 0.6);");
    m.insert("mute",
"mute(rx, bool) — mutes / unmutes audio.
  mute(radio.rx(0), true);");
    m.insert("lock",
"lock(rx, bool) — locks / unlocks the VFO.");
    m.insert("filter",
"filter(rx, lo, hi) — sets the passband in Hz.
  filter(radio.rx(0), 200.0, 2800.0);");
    m.insert("filter_preset",
"filter_preset(rx, name) — applies a filter preset.
  filter_preset(radio.rx(0), \"2.4K\");");
    m.insert("nr3",
"nr3(rx, bool) — enables RNNoise.");
    m.insert("nr4",
"nr4(rx, bool) — enables specbleach.");
    m.insert("nb",  "nb(rx, bool) — noise blanker.");
    m.insert("nb2", "nb2(rx, bool) — noise blanker 2.");
    m.insert("anf", "anf(rx, bool) — auto notch filter.");
    m.insert("bin", "bin(rx, bool) — binaural processing.");
    m.insert("tnf", "tnf(rx, bool) — tracking notch filter.");
    m.insert("rit",
"rit(rx, hz) — Receiver Incremental Tuning offset in Hz.
Display-only: paints a yellow vertical marker at center+rit_hz
on the spectrum. Clamped to ±10 kHz. Use `radio[0].rit = 250` or
the free function `rit(radio.rx(0), 250)`.");
    m.insert("agc",
"agc(rx, preset) — sets the AGC preset.
  agc(radio.rx(0), \"Fast\");");
    m.insert("eq",
"eq(rx, array11) — sets the 11 EQ gains.
  eq(radio.rx(0), [0,0,0,0,0,0,0,0,0,0,0]);");
    m.insert("eq_band",
"eq_band(rx, i, gain) — sets a single EQ band.");
    m.insert("band",
"band(s) — jumps to the given band.
  band(\"20\");");
    m.insert("tune",
"tune(hz) — sets the active RX frequency.");
    m.insert("active_rx",
"active_rx(i) — selects the active RX.");
    m.insert("num_rx",
"num_rx(n) — requests n RX (1 or 2).");
    m.insert("connect",
"connect() — starts the HPSDR handshake.");
    m.insert("disconnect",
"disconnect() — closes the session.");
    m.insert("memory_save",
"memory_save(name) — saves the active RX into memory.");
    m.insert("memory_load",
"memory_load(i) — loads memory i.");
    m.insert("memory_delete",
"memory_delete(i) — removes memory i.");
    m.insert("save",
"save() — forces immediate persistence of the settings.");
    m.insert("audio_device",
"audio_device(name) — changes the audio output device.");
    m.insert("window",
"window(kind, bool) — opens/closes a native window:
  window(\"memories\", true);

Or, with 3 arguments, declares a scripted window:
  window(\"panel\", \"Title\", || { label(\"x\"); });");
    m.insert("button",
"button(label, || { … }) — scripted button.
  button(\"40 m\", || { band(\"40\"); });");
    m.insert("slider",
"slider(label, key, min, max) — slider bound to a state key.
  slider(\"Vol\", \"vol\", 0.0, 1.0);");
    m.insert("checkbox",
"checkbox(label, key) — checkbox bound to a state key.");
    m.insert("on_change",
"on_change(key, |v| { … }) — callback triggered whenever the
value associated with `key` changes.");
    m.insert("menu_item",
"menu_item(\"Path/Item\", || { … }) — adds an entry to the
\"Scripts\" menu on the menu bar.");
    m.insert("help",
"help() — lists the available topics.
help(\"topic\") — shows the matching entry.");

    m
}
