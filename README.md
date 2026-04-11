# thetis-rust

Portage multi-plateforme en Rust de [Thetis](https://github.com/ramdor/Thetis), le logiciel
de contrôle pour les SDR Apache Labs (ANAN, Saturn, HermesLite 2). Le projet amont a été
archivé le 5 avril 2026 ; ce port vise Linux, macOS et Windows.

## État

Phase A — fondations + réception minimale HermesLite 2. Voir
`/home/jeff/.claude/plans/bright-juggling-parrot.md` pour la roadmap détaillée.

## Arbre

```
crates/
  wdsp-sys/         FFI vers la lib C WDSP (Warren Pratt DSP)
  wdsp/             Wrapper Rust sûr
  hpsdr-protocol/   Types de paquets HPSDR Protocol 1
  hpsdr-net/        Transport UDP, découverte, session radio
  thetis-audio/     Sortie audio cpal + ring buffers
  thetis-core/      State machine radio, orchestration I/O ↔ DSP
  thetis-settings/  Persistance TOML
  thetis-ui/        UI egui + wgpu
apps/
  thetis/           Binaire final
thetis-upstream/    Submodule : code source Thetis d'origine (référence)
```

## Dépendances système

- Rust stable ≥ 1.82
- FFTW3 (single & double precision) — `libfftw3-dev` / `fftw` selon la distro
- Un runtime audio supporté par [cpal](https://crates.io/crates/cpal) : ALSA (Linux),
  CoreAudio (macOS), WASAPI (Windows)
- Pour l'UI : bibliothèques Vulkan/OpenGL via wgpu

## Build

```sh
git clone --recurse-submodules <url> thetis-rust
cd thetis-rust
cargo build --workspace
```

## Licence

GPL-2.0-or-later, aligné sur Thetis amont.
