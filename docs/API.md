# Arion REST API

Arion exposes a JSON HTTP API under `/api/v1/*`, served by the
`arion-api` crate. It's the modern integration surface for scripts,
loggers, home-automation, and any tool that prefers `curl` to CAT or
MIDI.

## Enabling

Setup → Network → "Enable REST API". Default port **8081**, bound to
**127.0.0.1** only (edit the checkbox to expose on all interfaces —
read the warning first). Persisted in `arion.toml`.

No authentication is implemented. Exposing the API on a LAN without a
reverse proxy + auth is unsafe.

## Design

- **Resource-oriented URLs** — `/rx/{idx}/filter`, `/memories/{idx}`.
- **HTTP verbs** — `GET` reads, `PATCH` partial update, `PUT` full
  replacement, `POST` commands, `DELETE` removal.
- **Errors** — RFC 7807 flavour `{ type, title, status, detail, code }`.
- **Versioning** — URL prefix `/api/v1`. Breaking changes land under
  `/api/v2` without affecting v1 consumers.
- **Content type** — `application/json` only. Request bodies are
  strict JSON; unknown fields are rejected.

## Endpoint reference

### Instance & radio
| Method | Path | Purpose |
|---|---|---|
| GET  | `/api/v1/instance` | version, uptime, feature flags |
| GET  | `/api/v1/radio` | connection state, telemetry age |
| POST | `/api/v1/radio/connect` | `{ "ip"?: string }` |
| POST | `/api/v1/radio/disconnect` | |

### Receivers
| Method | Path | Purpose |
|---|---|---|
| GET  | `/api/v1/rx` | list all RX |
| GET  | `/api/v1/rx/{idx}` | single RX snapshot |
| PATCH | `/api/v1/rx/{idx}` | merge update — see full field list below |
| POST | `/api/v1/rx/{idx}/tune` | `{ "delta_hz": number }` (non-idempotent) |
| PATCH | `/api/v1/rx/{idx}/filter` | `{ "low": f64, "high": f64 }` |
| POST | `/api/v1/rx/{idx}/filter/preset` | `{ "preset": "F2700" }` |
| PATCH | `/api/v1/rx/{idx}/eq` | `{ "gains": [i32; 11] }` (preamp + 10 bands) |
| POST | `/api/v1/active-rx` | `{ "rx": usize }` |
| GET  | `/api/v1/rx/{idx}/tnf` | tracking-notch summary |
| POST | `/api/v1/rx/{idx}/tnf` | `{ "freq_hz": f64, "width_hz": f64, "active"?: bool }` |
| PUT  | `/api/v1/rx/{idx}/tnf/{nidx}` | replace notch |
| DELETE | `/api/v1/rx/{idx}/tnf/{nidx}` | remove notch |

#### `PATCH /rx/{idx}` body (all fields optional — only the ones present are applied)

| Field | Type | Purpose |
|---|---|---|
| `frequency_hz` | u32 | VFO frequency |
| `mode` | string | `USB / LSB / DSB / CWU / CWL / AM / FM / DIGU / DIGL / SAM / DRM / SPEC` |
| `volume` | f32 | AF gain |
| `muted` | bool | |
| `locked` | bool | |
| `rit_hz` | i32 | ±10 kHz clamp |
| `nr3` | bool | RNNoise |
| `nr4` | bool | libspecbleach |
| `anr` | bool | Thetis "NR" (LMS adaptive) |
| `emnr` | bool | Thetis "NR2" (enhanced spectral) |
| `agc` | string | `off / long / slow / med / fast` |
| `agc_max_gain_db` | f32 | AGC max-gain ceiling in dB (`max_gain = 10^(db/20)`; sane range 60..120) |
| `agc_decay_ms` | i32 | AGC decay time constant |
| `squelch` | bool | mode-dispatched squelch toggle |
| `squelch_db` | f32 | threshold (FM: 0..1 level; else dB) |
| `apf` | bool | Audio Peak Filter (CW) |
| `apf_freq_hz` | f32 | APF centre frequency |
| `fm_deviation_hz` | f32 | 2500 (narrow) or 5000 (wide) typical |
| `ctcss_on` | bool | FM tone squelch |
| `ctcss_hz` | f32 | CTCSS tone (67.0..254.1) |
| `sam_submode` | u8 | `0 = DSB`, `1 = LSB`, `2 = USB` |
| `bpsnba_nc` | u32 | BPSNBA FIR length (power of 2, ≥ 128) |
| `bpsnba_mp` | bool | BPSNBA minimum-phase flag |

### Bands
| Method | Path | Purpose |
|---|---|---|
| GET  | `/api/v1/bands` | list |
| POST | `/api/v1/bands/{band}` | jump (`M160..M6`) |

### Memories
| Method | Path | Purpose |
|---|---|---|
| GET  | `/api/v1/memories` | list |
| GET  | `/api/v1/memories/{idx}` | single entry |
| POST | `/api/v1/memories` | save current `{ rx, name, tag? }` |
| PUT  | `/api/v1/memories/{idx}` | replace entry |
| DELETE | `/api/v1/memories/{idx}` | remove |
| POST | `/api/v1/memories/{idx}/load` | apply to active RX |

### MIDI
| Method | Path | Purpose |
|---|---|---|
| GET  | `/api/v1/midi` | available devices + enable flag |
| PATCH | `/api/v1/midi` | `{ "enabled"?, "device_name"? }` |
| GET  | `/api/v1/midi/bindings` | current mapping table |
| POST | `/api/v1/midi/bindings` | append a binding |
| PUT  | `/api/v1/midi/bindings/{idx}` | replace a binding |
| DELETE | `/api/v1/midi/bindings/{idx}` | remove |
| GET  | `/api/v1/midi/last-event` | most recent CC / Note seen (learn mode) |

### External services
| Method | Path | Purpose |
|---|---|---|
| GET  | `/api/v1/rigctld` | hint text |
| PATCH | `/api/v1/rigctld` | `{ "enabled"?, "port"? }` |

### Scripting
| Method | Path | Purpose |
|---|---|---|
| POST | `/api/v1/scripts/eval` | `{ "source": "..." }` Rhai eval (gated) |

Disabled by default. Enable via Setup → Network → "Allow /scripts/eval".

### Observability
| Method | Path | Purpose |
|---|---|---|
| GET  | `/api/v1/telemetry` | full state + DSP telemetry JSON |
| GET  | `/api/v1/metrics` | Prometheus text format |

## Idempotence

- `GET`, `PUT`, `DELETE`, `PATCH` are idempotent.
- `POST /rx/{idx}/tune { delta_hz }` is **not** idempotent — each call
  adds `delta_hz` to the current VFO.

## Snapshot consistency

The state snapshot read by `GET` is republished roughly once per egui
frame (~60 Hz). A `GET /rx/0` made <16 ms after a `PATCH` may return
the pre-PATCH value. Poll again if you need post-action confirmation.

## Examples

```sh
# instance info
curl -s http://127.0.0.1:8081/api/v1/instance | jq

# QSY to 14.074 MHz USB on RX0
curl -X PATCH http://127.0.0.1:8081/api/v1/rx/0 \
     -H 'content-type: application/json' \
     -d '{"frequency_hz": 14074000, "mode": "USB"}'

# nudge frequency up 500 Hz
curl -X POST http://127.0.0.1:8081/api/v1/rx/0/tune \
     -H 'content-type: application/json' \
     -d '{"delta_hz": 500}'

# jump to 20m
curl -X POST http://127.0.0.1:8081/api/v1/bands/M20

# filter width preset
curl -X POST http://127.0.0.1:8081/api/v1/rx/0/filter/preset \
     -H 'content-type: application/json' \
     -d '{"preset": "F2700"}'

# scrape Prometheus
curl -s http://127.0.0.1:8081/api/v1/metrics
```

## Mode strings

`USB`, `LSB`, `DSB`, `CWU`, `CWL`, `AM`, `FM`, `DIGU`, `DIGL`, `SAM`,
`DRM`, `SPEC`. Stable across versions.

## Band labels

`M160`, `M80`, `M60`, `M40`, `M30`, `M20`, `M17`, `M15`, `M12`, `M10`, `M6`.

## Filter presets

`F6000`, `F4000`, `F2700`, `F2400`, `F1800`, `F1000`, `F600`, `F400`,
`F250`, `F100` (Hz widths).

## AGC presets

`off`, `long`, `slow`, `med`, `fast`.

## Out of scope (v1)

- **TX / PTT** — Phase C not implemented. Endpoints will land when
  `App::set_tx_enabled` exists.
- **Binary streams** (IQ, audio, spectrum) — use the `arion-web`
  WebSocket until TCI is wired.
- **UI state** (windows, tabs) — intentionally absent; the API only
  exposes the radio domain.
- **Get for BPSNBA / APF params** — WDSP exposes no reader for these
  knobs; values are write-only. Values applied via PATCH take effect
  but are not reflected in `GET /rx/{idx}` and (for BPSNBA) not
  persisted across restarts.

## OpenAPI spec

A hand-maintained OpenAPI 3.1 description lives at
[`openapi.yaml`](openapi.yaml). Suitable for Swagger UI, Postman
import, or generating client stubs in any supported language.
