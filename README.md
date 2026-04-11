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
- Un runtime audio supporté par [cpal](https://crates.io/crates/cpal) : ALSA (Linux),
  CoreAudio (macOS), WASAPI (Windows)
- Pour l'UI : bibliothèques Vulkan/OpenGL via wgpu

FFTW3, rnnoise et libspecbleach sont **vendorés** dans `crates/wdsp-sys/vendor*/`
et compilés par le `build.rs` via les crates `cmake` et `cc`. Aucune lib C
externe n'est nécessaire — `pkg-config` n'est plus requis.

## Build

```sh
git clone --recurse-submodules <url> thetis-rust
cd thetis-rust
cargo build --workspace
```

### Cross-compile Linux → Windows

Depuis une machine Linux, on peut produire un `thetis.exe` natif
x86_64 (PE32+) sans toucher à Windows :

```sh
# 1. Installer le compilateur C cross (Arch : pacman -S mingw-w64-gcc)
# 2. Ajouter la cible Rust
rustup target add x86_64-pc-windows-gnu
# 3. Compiler. Sur les distros où `/usr/bin/rustc` shadow rustup (Arch
#    notamment), forcer le PATH vers le rustc rustup-managed qui
#    connaît la sysroot windows-gnu :
PATH="$HOME/.cargo/bin:$PATH" \
  cargo build --target x86_64-pc-windows-gnu --release -p thetis
```

L'artefact final est `target/x86_64-pc-windows-gnu/release/thetis.exe`.
`wdsp-sys/build.rs` détecte le target et :
- construit FFTW 3.3.10 avec `WITH_OUR_MALLOC` (mingw n'a ni `posix_memalign`
  ni `memalign`),
- injecte `shim-win/Windows.h` pour corriger la casse (WDSP inclut
  `<Windows.h>`, w32api fournit `<windows.h>`),
- ne compile pas le shim POSIX (pthread / pseudo-Win32), le vrai w32api
  de mingw fournissant `CRITICAL_SECTION`, `_beginthread`, etc.
- linke contre `avrt` + `winmm` (MMCSS + timeBeginPeriod).

## Licence

GPL-2.0-or-later, aligné sur Thetis amont.
