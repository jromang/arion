# Arion — Digital modes guide

Arion can layer a digital decoder on top of any receiver so the
analog DSP path (filters, NR, AGC, waterfall) keeps working while
text / frames are pulled out of the demodulated audio.

| Mode   | Kind        | Carrier           | Baud    | Notes                                            |
|--------|-------------|-------------------|---------|--------------------------------------------------|
| PSK31  | BPSK text   | audio (user-set)  | 31.25   | QSO-grade HF text mode; narrow (~60 Hz)           |
| PSK63  | DPSK text   | audio (user-set)  | 62.5    | Faster PSK variant                                |
| RTTY   | FSK text    | 1445 / 1275 Hz    | 45.45   | ITA2 Baudot, 170 Hz shift                         |
| APRS   | AFSK frames | 1200 / 2200 Hz    | 1200    | Bell 202, HDLC + AX.25 UI frames                  |
| FT8    | 8-FSK text  | audio (movable)   | 6.25    | UTC-aligned 15-s slots, LDPC FEC                  |
| WSPR   | 4-FSK text  | ~1400 Hz          | 1.46    | UTC-aligned 120-s slots. **Decoder WIP.**         |

Audio taps the DSP thread *after* AGC, so any passband / NR / notch
adjustments you make in the regular DSP chain apply before the
digital decoder sees the audio.

## Activating a decoder

### egui desktop

1. Pick the right analog mode first — USB/DIGU for most HF digital
   modes, LSB/DIGL below 10 MHz, FM for APRS on VHF/UHF.
2. `View → Digital Decodes` opens a floating window with a mode
   dropdown (Off / PSK31 / PSK63 / RTTY / APRS / FT8 / WSPR).
3. For PSK31 / PSK63, a "Carrier" slider (300–2700 Hz) selects the
   audio offset inside the passband. **Ctrl+click** a signal on
   the spectrum to retune the carrier directly to that pixel
   (works on USB, DIGU, LSB, DIGL).
4. `View → Constellation` paints the last 256 post-symsync I/Q
   points for PSK — two tight clusters on ±I is a clean BPSK
   lock, a smeared donut means sync is hunting.

### ratatui console (`arion-tui`)

The side panel's fourth section ("Digital") mirrors the desktop
dropdown. `Tab` cycles focus to it, ↑/↓ pick a mode, Enter
activates. A "Decodes" pane below logs the last 64 messages with
a per-mode tag.

### REST (JSON, port 8081)

```
# Enable FT8 on RX 0
curl -X PATCH http://localhost:8081/api/v1/rx/0 \
     -H 'Content-Type: application/json' \
     -d '{"digital_mode":"ft8"}'

# Retune the PSK carrier to 1200 Hz
curl -X PATCH http://localhost:8081/api/v1/rx/0 \
     -H 'Content-Type: application/json' \
     -d '{"digital_mode":"psk31","digital_center_hz":1200}'

# Read back the current state including live decodes
curl http://localhost:8081/api/v1/state | jq '.rx[0] |
    {digital_mode, digital_center_hz, digital_decodes}'
```

Full schema in `docs/openapi.yaml` (`RxPatch`, `RxSnapshot`,
`DigitalDecodeSnapshot`).

### Rhai scripting

```rhai
// Enable PSK31 at the traditional 1000 Hz spot
radio.rx(0).digital_mode = "psk31";
radio.rx(0).digital_center_hz = 1000.0;

// Pretty-print the live decodes
for d in radio.rx(0).digital_decodes() {
    print(`${d.mode} ${d.freq}Hz: ${d.text}`);
}

// Grab a constellation snapshot (PSK only)
let pts = radio.rx(0).constellation();
print(`got ${pts.len()} constellation points`);
```

Digital settings persist across sessions in
`~/.config/arion/arion.toml` under the `rxs.[].digital_mode` and
`rxs.[].digital_center_hz` fields.

## Mode tips

### PSK31 / PSK63

- Symbol sync is done by liquid's `SymSync` (Kaiser polyphase,
  32-bank, lf_bw=0.05) — robust to signals that start mid-symbol.
- The first character of a new signal is often lost during
  acquisition; this matches fldigi behaviour.
- Idle carrier is decoded as zero bits (continuous phase
  reversals) and produces no output, so a station's "CQ" will
  only be emitted once they actually start transmitting text.

### RTTY

- Hardcoded to **45.45 baud / 170 Hz shift / mark=1445 Hz**
  (amateur standard). The two tones must be in the passband;
  with a 2.4 kHz SSB filter centred on 1500 Hz that is automatic.
- Baudot LTRS ↔ FIGS shifts are tracked per message.

### APRS

- Only the VHF Bell-202 flavour (AFSK 1200 bd, mark=1200, space=2200)
  is supported. Set RX to FM and 25 kHz filter.
- UI frames are parsed (source, destination, info field).
  Digipeater chains / compressed position formats are not parsed
  yet — they appear verbatim in the info field.
- Any HDLC-level error (CRC mismatch, bad stuffing) drops the
  frame silently.

### FT8

- The RX window scans the full audio passband, so every decodable
  FT8 signal in the band comes out — no per-signal tuning
  needed. Decodes carry `score` (ft8_lib sync score, rank-only),
  `freq_hz` (where the signal sat), and `time_offset_s`.
- Decoding is triggered at each UTC 15-second slot boundary
  (xx:00, xx:15, xx:30, xx:45). A rolling 14-second trigger is
  the fallback when the wall-clock is unavailable (tests).
- Repeated candidates producing the same text are deduplicated.

### WSPR

- **Decoder integration is WIP** (phase G.1 decoder step). The
  mode selector, slot timing (120-s UTC-aligned), resampling
  (48 → 375 Hz) and pipeline plumbing are in place; what is
  still missing is the Fano-decoded payload extraction from
  WSJT-X's `wsprd`. The scaffold in `crates/wsprd-sys/` vendors
  the C sources; see `crates/arion-core/src/digital/wspr.rs`
  for the stub site.

## External digital-mode clients

JS8Call, WSJT-X, fldigi, and similar standalone clients are best
run **alongside** Arion — point them at Arion's rigctld port
(`:4532` by default) for CAT control, and route Arion's audio to
them through an OS-level loopback (`snd-aloop` on Linux,
BlackHole on macOS, VB-Audio Cable on Windows). The built-in
decoders above are an alternative, not a replacement.

## Limitations & roadmap

- **RX only.** Every mode listed above is a receiver. TX belongs
  to phase E (TX scaffolding) — once that lands, `ft8::encode_to_audio`
  and the RTTY / PSK / APRS encoders already in-tree can be
  wired in.
- **No true SNR in dB** for FT8 / WSPR — the `score` field in
  `DigitalDecode` is ft8_lib's sync score (monotonic with SNR,
  not calibrated). A real SNR estimator requires extra
  instrumentation in the decoder.
- **No JS8 decoder in-tree** — JS8Call's decoder is tightly
  coupled to its Qt codebase. Use the external-client path above.
- **No FreeDV voice** yet — planned, tracked in
  `todo/other_modes.md`. Needs a new audio-out channel in the
  DigitalPipeline (voice, not text).
