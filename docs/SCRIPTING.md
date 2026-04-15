# Rhai scripting in Arion

Arion embeds a [Rhai](https://rhai.rs) script engine that lets you
drive the radio, automate repetitive tasks, and build custom
interface panels — without recompiling.

This document covers:

1. [Introduction](#1-introduction)
2. [Quick start](#2-quick-start)
3. [The `radio` object](#3-the-radio-object)
4. [Action functions](#4-action-functions)
5. [Modes, bands, filters, AGC](#5-modes-bands-filters-agc)
6. [Memories & band stack](#6-memories--band-stack)
7. [Building a UI](#7-building-a-ui)
8. [Startup script](#8-startup-script)
9. [Examples](#9-examples)
10. [Appendix — full API reference](#10-appendix--full-api-reference)

---

## 1. Introduction

Scripting in Arion serves three purposes:

- **Drive the radio from the command line**: REPL in the *Scripts*
  window (menu *View → Scripts* or configured key).
- **Edit, save and load scripts**: tabs in the same window, with
  syntax highlighting.
- **Run a script at startup**: `~/.config/arion/startup.rhai` is
  loaded automatically when egui launches (see §8).

The engine runs **on the UI thread**: each `run_line` is short and
capped at one million Rhai operations per call to keep a buggy
script from freezing the interface.

> **REPL tip** — `help()` lists the available topics,
> `help("topic")` shows the details.

---

## 2. Quick start

Open the *Scripts* window and type, line by line:

```rhai
radio.connect();                    // open the HPSDR session
radio[0].freq = 14074000;           // RX0 → 14.074 MHz (FT8)
radio[0].mode = "USB";
radio[0].volume = 0.5;
```

You can also use the more concise free functions:

```rhai
freq(radio.rx(0), 7074000);
mode(radio.rx(0), "LSB");
band("40");
```

---

## 3. The `radio` object

`radio` is injected as a **constant** into the scope of every call.
It is the only gateway to the application's state.

### 3.1 Properties (read)

| Property           | Type      | Meaning                                   |
|---|---|---|
| `radio.connected`  | `bool`    | true if the HPSDR session is active       |
| `radio.ip`         | `String`  | effective radio IP                        |
| `radio.num_rx`     | `int`     | number of requested RX (1 or 2)           |
| `radio.active_rx`  | `int`     | index of the active RX                    |
| `radio.last_error` | `String`  | last observed error (empty if none)       |

### 3.2 Methods

| Method               | Description                                   |
|---|---|
| `radio.rx(i)`        | Read access to RX `i` (for use as argument)   |
| `radio[i]`           | Write access to RX `i` (see box)              |
| `radio.connect()`    | Starts discovery + handshake                  |
| `radio.disconnect()` | Closes the session                            |
| `radio.save()`       | Forces settings persistence                   |
| `radio.tick()`       | Pulses an application tick (rarely useful)    |

> **Convention `radio[i].field = …` vs `radio.rx(i).field`**
>
> Rhai 1.x refuses writing to the left of a method result
> (`radio.rx(0).freq = v` → error). However it accepts
> **indexers**. So we expose `rx()` for reads and `[i]` for writes:
>
> ```rhai
> let f = radio.rx(0).freq;      // read
> radio[0].freq = 14074000;      // write
> ```

### 3.3 RX properties

| Property        | Type       | R/W | Detail                                |
|---|---|---|---|
| `.freq`         | `int`      | R/W | Frequency in Hz                       |
| `.mode`         | `String`   | R/W | See §5                                |
| `.volume`       | `float`    | R/W | 0.0..1.0                              |
| `.muted`        | `bool`     | R/W |                                       |
| `.locked`       | `bool`     | R/W | VFO lock                              |
| `.enabled`      | `bool`     | R/W |                                       |
| `.filter_lo`    | `float`    | R/W | Hz                                    |
| `.filter_hi`    | `float`    | R/W | Hz                                    |
| `.nr3`, `.nr4`  | `bool`     | R/W | RNNoise / specbleach                  |
| `.anr`, `.emnr` | `bool`     | R/W | LMS adaptive NR / enhanced spectral NR |
| `.agc`          | `String`   | R/W | `"Off"`, `"Long"`, `"Slow"`, `"Med"`, `"Fast"` |
| `.agc_top_dbm`  | `float`    | R/W | AGC top level in dBm                  |
| `.agc_hang_level` | `float`  | R/W | AGC hang level                        |
| `.agc_decay_ms` | `int`      | R/W | AGC decay time constant (ms)          |
| `.agc_fixed_gain` | `float`  | R/W | Gain when AGC=Off (dB)                |
| `.nb`, `.nb2`   | `bool`     | R/W | Noise blankers                        |
| `.anf`          | `bool`     | R/W | Auto notch filter                     |
| `.bin`          | `bool`     | R/W | Binaural                              |
| `.tnf`          | `bool`     | R/W | Tracking notch master switch          |
| `.squelch`      | `bool`     | R/W | Mode-dispatched squelch toggle        |
| `.squelch_db`   | `float`    | R/W | Threshold (dB for SSB/AM, 0..1 level for FM) |
| `.apf`          | `bool`     | R/W | Audio Peak Filter (CW)                |
| `.apf_freq_hz`  | `float`    | R/W | APF centre Hz                         |
| `.apf_bw_hz`    | `float`    | R/W | APF bandwidth Hz                      |
| `.apf_gain_db`  | `float`    | R/W | APF gain dB                           |
| `.fm_deviation_hz` | `float` | R/W | 2500 (narrow) or 5000 (wide) typical  |
| `.ctcss`        | `bool`     | R/W | FM CTCSS tone squelch                 |
| `.ctcss_hz`     | `float`    | R/W | CTCSS frequency 67.0..254.1           |
| `.sam_submode`  | `int`      | R/W | SAM: 0=DSB, 1=LSB, 2=USB              |
| `.rit`          | `int`      | R/W | Receiver Incremental Tuning (Hz, ±10 kHz), display-only |
| `.eq_enabled`   | `bool`     | R/W |                                       |
| `.eq_gains`     | `Array<i>` | R/W | 11 integer gains                      |
| `.s_meter`      | `float`    | R   | dB                                    |
| `.spectrum`     | `Array<f>` | R   | Spectrum snapshot                     |
| `.center_freq`  | `int`      | R   | Hz                                    |

---

## 4. Action functions

The free functions exist for brevity in the REPL. Each one has an
equivalent property.

### 4.1 Frequency / mode / volume

```rhai
freq(rx, hz);          mode(rx, s);          volume(rx, v);
mute(rx, bool);        lock(rx, bool);
tune(hz);              // active RX
```

### 4.2 Filters

```rhai
filter(rx, lo, hi);                   // Hz
filter_preset(rx, "2.4K");            // see §5
```

### 4.3 Noise reduction

Four independent reducers, combinable:

```rhai
nr3(rx, true);    nr4(rx, false);     // RNNoise / specbleach
anr(rx, true);    emnr(rx, false);    // ANR (Thetis NR) / EMNR (Thetis NR2)
nb(rx, true);     nb2(rx, false);     // time-domain blankers
anf(rx, true);    bin(rx, true);      // auto notch, binaural
tnf(rx, true);                        // tracking notch master switch
tnf_add(rx, 1500.0, 50.0);            // add a notch @1500 Hz, 50 Hz wide
tnf_delete(rx, 0);                    // remove notch index 0
```

### 4.3b Squelch (mode-dispatched: SSB / AM / FM)

```rhai
squelch(rx, true);
squelch_db(rx, -30.0);      // dB threshold (FM: 0..1 level)
```

### 4.3c APF (CW Audio Peak Filter)

```rhai
apf(rx, true);
// Fine-tune via Rx properties:
radio[0].apf_freq_hz = 600.0;
radio[0].apf_bw_hz   = 50.0;
radio[0].apf_gain_db = 6.0;
```

### 4.3d FM + CTCSS

```rhai
ctcss(rx, true);
ctcss_hz(rx, 67.0);
radio[0].fm_deviation_hz = 2500.0;   // narrow; 5000.0 = wide
```

### 4.3e BPSNBA tuning (advanced)

BPSNBA auto-routes when SNBA + TNF are both active. Only the FIR
filter length and phase shape are tunable — write-only, not
persisted (defaults restore on every boot).

```rhai
bpsnba_nc(rx, 4096);    // power-of-2, ≥ 128 (default 2048)
bpsnba_mp(rx, false);   // true = minimum phase, false = linear
```

### 4.4 AGC

```rhai
agc(rx, "Fast");
```

### 4.4b RIT (Receiver Incremental Tuning)

Shifts the receive frequency by ±hz without moving the VFO. Drawn as a
yellow vertical marker on the spectrum at `center_freq + rit_hz`. Set
to `0` to hide the marker. Clamped to ±10 kHz. Display-only for now —
the WDSP wiring will follow once the TX path lands.

```rhai
radio[0].rit = 500;       // receive 500 Hz above the VFO
rit(radio.rx(0), -250);   // free-function form
radio[0].rit = 0;         // clear
```

### 4.5 EQ

```rhai
eq(rx, [0, 1, 2, 3, 4, 5, 4, 3, 2, 1, 0]);   // 11 gains
eq_band(rx, 5, -3);                           // a single band
```

### 4.6 Band / RX / application

```rhai
band("20");
active_rx(1);       num_rx(2);
connect();          disconnect();
save();             audio_device("pulse");
```

### 4.7 Memories

```rhai
memory_save("DX cluster");
memory_load(0);
memory_delete(2);
```

### 4.8 Native windows

```rhai
window("memories", true);
window("setup", false);
```

---

## 5. Modes, bands, filters, AGC

### Modes (case-insensitive)

`LSB, USB, DSB, CWL, CWU (alias CW), FM, AM, DIGU, DIGL, SAM, SPEC, DRM`.

### Bands

`"160", "80", "60", "40", "30", "20", "17", "15", "12", "10", "6"`.

### Filters

| Preset | Equivalent                       |
|---|---|
| `"6.0K"` / `"6K"` | 6000 Hz              |
| `"4.0K"` / `"4K"` | 4000 Hz              |
| `"2.7K"`          | 2700 Hz              |
| `"2.4K"`          | 2400 Hz              |
| `"1.8K"`          | 1800 Hz              |
| `"1.0K"` / `"1K"` | 1000 Hz              |
| `"600"`           | 600 Hz               |
| `"400"`           | 400 Hz               |
| `"250"`           | 250 Hz               |
| `"100"`           | 100 Hz               |

### AGC

`"Off", "Long", "Slow", "Med"` (alias `"Medium"`), `"Fast"`.

---

## 6. Memories & band stack

Memories are named and persistent. The **band stack** is managed
automatically: every time you leave a band, the current frequency
is pushed onto the stack; `band("X")` restores the last entry.

```rhai
freq(radio.rx(0), 10123000);
memory_save("RBN WSPR");

band("80");          // jump to 80m at the last known freq
memory_load(0);      // return to the "RBN WSPR" entry
```

---

## 7. Building a UI

The UI API is declarative: a `window(id, title, body)` call
evaluates `body` (a `||{…}` closure) and builds a tree of
`Widget`s. The egui renderer (on the application side) translates
that tree into native widgets every frame.

### 7.1 Containers

```rhai
window("panel", "My panel", || {
    label("RX0 frequency:");
    hbox(|| {
        button("-1 kHz", || { radio[0].freq = radio.rx(0).freq - 1000; });
        button("+1 kHz", || { radio[0].freq = radio.rx(0).freq + 1000; });
    });
    separator();
    slider("Volume", "vol", 0.0, 1.0);
    checkbox("NR3", "nr3");
});
```

### 7.2 Reacting to changes

```rhai
on_change("vol", |v| { radio[0].volume = v; });
on_change("nr3", |b| { radio[0].nr3    = b; });
```

### 7.3 "Scripts" menu

```rhai
menu_item("Favorites/FT8 20 m", || {
    band("20");
    freq(radio.rx(0), 14074000);
    mode(radio.rx(0), "USB");
});
```

### 7.4 Controlling scripted windows

```rhai
window_show("panel");
window_hide("panel");
window_toggle("panel");
```

---

## 8. Startup script

When egui launches, Arion automatically loads the file:

| OS      | Path                                              |
|---------|---------------------------------------------------|
| Linux   | `~/.config/arion/startup.rhai`                    |
| macOS   | `~/Library/Application Support/arion/startup.rhai`|
| Windows | `%APPDATA%\arion\startup.rhai`                    |

The file is executed **once**, in the main engine's scope. The
windows, menus and callbacks it declares persist until the
application closes.

If the file is missing → silent. A read or evaluation error is
logged (via `tracing`) and shown as an error line in the REPL —
open the *Scripts* window to view it.

---

## 9. Examples

See `examples/scripts/`:

| File                           | Topic                              |
|---|---|
| `01_basics.rhai`               | Connect, tune, mode, volume        |
| `02_scan.rhai`                 | Memory scan                        |
| `03_custom_panel.rhai`         | Panel with buttons + slider        |
| `04_macro_button.rhai`         | "Macro" menu entry                 |
| `05_menu_extension.rhai`       | Multiple actions in the menu       |
| `06_ui_complete.rhai`          | Full UI, startup.rhai style        |

---

## 10. Appendix — full API reference

### 10.1 `radio` root

| Symbol             | Signature                      | R/W |
|---|---|---|
| `radio.connected`  | `bool`                         | R   |
| `radio.ip`         | `String`                       | R   |
| `radio.num_rx`     | `int`                          | R   |
| `radio.active_rx`  | `int`                          | R   |
| `radio.last_error` | `String`                       | R   |
| `radio.rx(i)`      | `(int) -> Rx`                  | —   |
| `radio[i]`         | `(int) -> Rx` (indexer)        | —   |
| `radio.connect()`  | `() -> ()`                     | —   |
| `radio.disconnect()`| `() -> ()`                    | —   |
| `radio.save()`     | `() -> ()`                     | —   |
| `radio.tick()`     | `() -> ()`                     | —   |

### 10.2 RX properties — see §3.3.

### 10.3 Free functions

| Name              | Signature                             |
|---|---|
| `freq`            | `(Rx, int)`                           |
| `mode`            | `(Rx, String)`                        |
| `volume`          | `(Rx, float)`                         |
| `mute`            | `(Rx, bool)`                          |
| `lock`            | `(Rx, bool)`                          |
| `tune`            | `(int)`                               |
| `filter`          | `(Rx, float, float)`                  |
| `filter_preset`   | `(Rx, String)`                        |
| `nr3`/`nr4`/`nb`/`nb2`/`anf`/`bin`/`tnf` | `(Rx, bool)`           |
| `rit`             | `(Rx, int)`                           |
| `agc`             | `(Rx, String)`                        |
| `eq`              | `(Rx, Array)`                         |
| `eq_band`         | `(Rx, int, int)`                      |
| `band`            | `(String)`                            |
| `active_rx`       | `(int)`                               |
| `num_rx`          | `(int)`                               |
| `connect`         | `()`                                  |
| `disconnect`      | `()`                                  |
| `memory_save`     | `(String)`                            |
| `memory_load`     | `(int)`                               |
| `memory_delete`   | `(int)`                               |
| `window`          | `(String, bool)` or `(String, String, Fn)` |
| `save`            | `()`                                  |
| `audio_device`    | `(String)`                            |
| `help`            | `()` or `(String)` → `String`         |

### 10.4 Scripted UI

| Name             | Signature                                      |
|---|---|
| `window`         | `(id, title, body: Fn)`                        |
| `vbox` / `hbox`  | `(body: Fn)`                                   |
| `label`          | `(String)`                                     |
| `button`         | `(String, Fn)`                                 |
| `slider`         | `(label, key, min, max)`                       |
| `checkbox`       | `(label, key)`                                 |
| `text_edit`      | `(key)`                                        |
| `separator`      | `()`                                           |
| `on_change`      | `(key, Fn(v))`                                 |
| `menu_item`      | `(path, Fn)`                                   |
| `window_show`    | `(id)`                                         |
| `window_hide`    | `(id)`                                         |
| `window_toggle`  | `(id)`                                         |

---

*Last update: phase 6 of the scripting work (see `PLAN.md`).*
