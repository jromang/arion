# MIDI controllers

Arion turns any USB MIDI controller (knobs, pads, encoders, faders)
into a radio control surface. Tune the VFO with a real knob,
slam band buttons with pads, toggle NR with a footswitch.

## Enabling

1. Plug in the MIDI device before launching Arion.
2. Open *Setup → MIDI*.
3. Check **Enable MIDI input**.
4. Pick the device from the dropdown. Click **Rescan** if a device
   was plugged in after launch.
5. The **Learn** section shows the last event received — move a
   control on your device to confirm the connection.

Settings persist in `arion.toml` (`[midi]` section). Bindings live in
a separate file, `~/.config/arion/midi.toml` (or the platform
equivalent), so you can source-control presets independently of the
main settings.

## Quick start — ready-made presets

Two presets ship in [`docs/midi-presets/`](midi-presets/) :

- [`beatstep.toml`](midi-presets/beatstep.toml) — Arturia BeatStep,
  with the big Rate/Fine knob mapped to RX0 VFO (10 Hz / tick),
  encoders on AF gain / RIT, pads on band jumps and mode selection
- [`x-touch-mini.toml`](midi-presets/x-touch-mini.toml) — Behringer
  X-Touch Mini, encoder 1 on AF gain, encoder 2 on RIT delta,
  top-row buttons on band jumps

Copy the chosen preset to `~/.config/arion/midi.toml` and restart
Arion. Edit in place; Arion hot-swaps the mapping table without
restarting the listener.

## Binding format

Each binding has three parts:

### Trigger — what MIDI event fires it

```toml
# Control Change (potentiometer, encoder, fader)
trigger = { kind = "cc", channel = 0, controller = 7 }

# Note On (button, pad)
trigger = { kind = "note", channel = 0, note = 36 }
```

### Scale — how the 7-bit value is interpreted

```toml
# Absolute 0..127 → linear map to [min, max]
scale = { kind = "absolute", min = 0.0, max = 1.0 }

# Endless encoder (Mackie 2's-complement convention):
#   1..63  → CW (+value)
#   65..127 → CCW (-(value-64))
# `step` scales each tick (e.g. 10 Hz per click for VFO)
scale = { kind = "relative", step = 10.0 }

# Button: fires on Note On, value ignored
scale = { kind = "trigger" }
```

### Target — what Arion action to perform

```toml
target = { kind = "volume", rx = 0 }
target = { kind = "frequency", rx = 0 }        # use with relative scale
target = { kind = "rit", rx = 0 }
target = { kind = "mode", rx = 0, mode = "Usb" }
target = { kind = "filter_preset", rx = 0, preset = "F2700" }
target = { kind = "agc", rx = 0, agc = "Fast" }
target = { kind = "band", band = "M20" }
target = { kind = "memory", idx = 3 }
target = { kind = "active_rx", rx = 1 }
target = { kind = "toggle_flag", rx = 0, flag = "nr3" }
target = { kind = "ptt" }                      # no-op until TX lands
```

Valid `toggle_flag` keys mirror [`App::toggle_rx_flag`](../crates/arion-app/src/lib.rs) :
`nb`, `nb2`, `anf`, `bin`, `tnf`, `nr3`, `nr4`, `anr`, `emnr`,
`mute`, `lock`, `eq`.

## Encoder types — relative vs absolute

Most MIDI potentiometers are **absolute** — they send CC 0..127 based
on the physical position. Arion's `Absolute` scale linearly maps the
range to the target value.

Endless encoders (no physical stop) are **relative** — they send a
delta per click. Arion uses the Mackie convention (CW = 1..63, CCW =
65..127). Configure your controller editor (MCP / Midi Control
Editor / MIDI Control Center) to emit relative CC if you want to
drive the VFO smoothly.

## Learn mode

The Setup → MIDI tab always shows the **last event received**.
Watching this readout while moving a control is the fastest way to
figure out which CC / Note number your device emits — then hand-edit
`midi.toml`.

## Hot-swap

Edits to the mapping table (via the REST API, or a future UI editor)
swap atomically through `Arc<ArcSwap<MappingTable>>`. The MIDI
listener thread reads the latest table on every event — no restart,
no lost messages during the swap.

## REST API

Mapping CRUD is exposed in the REST API — see
[`docs/API.md`](API.md#midi) :

```sh
# List current bindings
curl -s http://127.0.0.1:8081/api/v1/midi/bindings | jq

# Peek the last raw event (useful to script Learn mode)
curl -s http://127.0.0.1:8081/api/v1/midi/last-event | jq

# Add a binding: VFO tuning on CC 16
curl -X POST http://127.0.0.1:8081/api/v1/midi/bindings \
     -H 'content-type: application/json' \
     -d '{
       "trigger": { "kind": "cc", "channel": 0, "controller": 16 },
       "scale":   { "kind": "relative", "step": 10.0 },
       "target":  { "kind": "frequency", "rx": 0 }
     }'
```

## Env-var override (CI / scripted runs)

Setting `ARION_MIDI_DEVICE=<substring>` at launch auto-enables MIDI
and opens the first device whose name contains the substring,
regardless of the persisted settings. Useful for headless smoke
tests or a repeatable runbook.

## Gotchas

- **Linux** : `midir` uses ALSA sequencer. If your user isn't in the
  `audio` group, the port may fail to open.
- **Hot-plug** : unplug + replug without clicking **Rescan** means
  the listener is still holding the stale port. Click Rescan (or
  restart Arion) to re-enumerate.
- **Multiple devices** : only one MIDI input is open at a time
  today. To combine two surfaces, use your OS's virtual MIDI router
  (e.g. ALSA MIDI Through) to merge them into one stream.
