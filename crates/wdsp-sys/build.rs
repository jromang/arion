//! Build script for `wdsp-sys`.
//!
//! Pipeline:
//! 1. Build FFTW 3.3.8 from the vendored sources in `vendor/fftw-3.3.10/`,
//!    once in double precision and once in single precision, via the
//!    upstream CMakeLists. This removes the `pkg-config` dep on
//!    `fftw3` / `fftw3f` and makes the build cross-compilable: the
//!    `cmake` cargo crate injects `CMAKE_SYSTEM_NAME` and picks the
//!    right C toolchain for the target (works with mingw-w64-gcc for
//!    Linux→Windows out of the box).
//! 2. Locate upstream WDSP sources in the `thetis-upstream/` submodule.
//! 3. Stage a fresh copy into `$OUT_DIR/wdsp/` on every build.
//! 4. Apply every `patches/*.patch` in lexicographic order via `patch -p1`.
//! 5. Compile the staged sources with `cc`, intercepting Windows-only
//!    includes (`<Windows.h>`, `<process.h>`, `<intrin.h>`, `<avrt.h>`)
//!    with the stub headers under `shim/`, and pointing `-I` at the
//!    FFTW install tree we just built.
//! 6. Link FFTW3 (single + double precision) statically from the
//!    vendored build.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Explicit list of WDSP `.c` sources we compile. Not a glob — adding or
/// removing files has to show up in source control. `rnnr.c` / `sbnr.c` are
/// replaced by `shim/wdsp_nr_stubs.c` in phase A.
const WDSP_C_SOURCES: &[&str] = &[
    "amd.c", "ammod.c", "amsq.c", "analyzer.c", "anf.c", "anr.c",
    "apfshadow.c", "bandpass.c", "calcc.c", "calculus.c", "cblock.c",
    "cfcomp.c", "cfir.c", "channel.c", "cmath.c", "compress.c", "delay.c",
    "dexp.c", "div.c", "doublepole.c", "eer.c", "emnr.c", "emph.c", "eq.c",
    "fcurve.c", "FDnoiseIQ.c", "fir.c", "firmin.c", "fmd.c", "fmmod.c",
    "fmsq.c", "gain.c", "gaussian.c", "gen.c", "icfir.c", "iir.c",
    "impulse_cache.c", "iobuffs.c", "iqc.c", "lmath.c", "main.c",
    "matchedCW.c", "meter.c", "meterlog10.c", "nbp.c", "nob.c", "nobII.c",
    "osctrl.c", "patchpanel.c", "resample.c", "rmatch.c", "RXA.c",
    "sender.c", "shift.c", "siphon.c", "slew.c", "snb.c", "ssql.c",
    "syncbuffs.c", "TXA.c", "utilities.c", "varsamp.c", "version.c",
    "wcpAGC.c", "wisdom.c", "zetaHat.c",
];

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir      = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let shim_dir     = manifest_dir.join("shim");
    let shim_win_dir = manifest_dir.join("shim-win");
    let patches_dir  = manifest_dir.join("patches");
    // Upstream WDSP lives two levels up in the submodule. The path has a
    // literal space — PathBuf handles it fine; the `patch` subprocess is
    // invoked via exec without a shell so no quoting is required.
    let upstream_wdsp = manifest_dir
        .parent().unwrap()          // crates/
        .parent().unwrap()          // thetis-rust/
        .join("thetis-upstream")
        .join("Project Files")
        .join("Source")
        .join("wdsp");

    if !upstream_wdsp.is_dir() {
        panic!(
            "Upstream WDSP source not found at {:?}. \
             Did you forget `git submodule update --init`?",
            upstream_wdsp
        );
    }

    // --- FFTW3 (vendored, both precisions) -----------------------------
    //
    // Upstream WDSP uses `fftwf_*` (single) and `fftw_*` (double) —
    // notably emnr and cfcomp run double-precision overlap-save FFTs.
    // Both precisions are built from the same source tree by running
    // cmake twice with `ENABLE_FLOAT` toggled.
    //
    // The cmake crate installs each build into a distinct prefix so
    // the single-precision pass doesn't clobber the double-precision
    // one. We then tell the linker about both install dirs.
    let fftw_include = build_fftw(&manifest_dir);

    // --- NR libraries (vendored) ---------------------------------------
    //
    // `rnnoise` (NR3) and `libspecbleach` (NR4) are built from the
    // vendored sources under `vendor-nr/`. The `nr` cargo feature
    // (default on) controls whether they're compiled at all; when
    // disabled, the stubs in `shim/wdsp_nr_stubs.c` provide no-op
    // symbols so WDSP still links. No runtime pkg-config probe, no
    // system install required — the build is fully self-contained.
    let nr_enabled = cfg!(feature = "nr");
    let rnnoise_include   = build_rnnoise(&manifest_dir, nr_enabled);
    let specbleach_include = build_specbleach(&manifest_dir, nr_enabled, &fftw_include);

    // --- Stage a fresh copy of upstream sources into OUT_DIR ------------
    let staged_dir = out_dir.join("wdsp");
    stage_upstream(&upstream_wdsp, &staged_dir);

    // --- Apply patches in lexicographic order ---------------------------
    apply_patches(&patches_dir, &staged_dir);

    // --- Cargo rerun triggers -------------------------------------------
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", shim_dir.display());
    println!("cargo:rerun-if-changed={}", shim_win_dir.display());
    println!("cargo:rerun-if-changed={}", patches_dir.display());
    println!("cargo:rerun-if-changed={}", upstream_wdsp.display());

    // --- Compile --------------------------------------------------------
    //
    // Include-path ordering:
    //   1. Vendored NR headers (when `nr` feature is on), so the real
    //      `rnnoise.h` / `specbleach_adenoiser.h` win.
    //   2. On non-Windows: `shim/` intercepts `<Windows.h>` /
    //      `<process.h>` / `<intrin.h>` / `<avrt.h>` with POSIX
    //      stubs. On Windows (mingw-w64) we skip the shim entirely and
    //      let the w32api headers provide the real definitions — WDSP
    //      was originally Win32, so on mingw it builds almost natively.
    //      The shim itself `#error`s if `_WIN32` is defined, which is
    //      why we have to gate its include path.
    //   3. The staged WDSP source dir.
    //   4. FFTW vendored include.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_is_windows = target_os == "windows";

    let mut build = cc::Build::new();
    if nr_enabled {
        if let Some(p) = rnnoise_include.as_ref()  { build.include(p); }
        if let Some(p) = specbleach_include.as_ref() { build.include(p); }
    }
    if target_is_windows {
        // Case-correcting `Windows.h` → `<windows.h>` forwarder for
        // mingw-w64 (w32api ships lowercase filenames). The rest of
        // the POSIX shim is not needed — w32api provides the real
        // types on Windows targets.
        build.include(&shim_win_dir);
    } else {
        build.include(&shim_dir);
    }
    build
        .include(&staged_dir)
        .include(&fftw_include)
        .flag_if_supported("-std=c11")
        .flag_if_supported("-fvisibility=default")
        .flag_if_supported("-fno-strict-aliasing")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-variable")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-unused-function")
        .flag_if_supported("-Wno-implicit-function-declaration")
        .flag_if_supported("-Wno-incompatible-pointer-types")
        .flag_if_supported("-Wno-pointer-sign")
        .flag_if_supported("-Wno-parentheses")
        .flag_if_supported("-Wno-maybe-uninitialized")
        .flag_if_supported("-Wno-misleading-indentation")
        .flag_if_supported("-Wno-sign-compare")
        // Expose POSIX.1-2008 / GNU extensions (PTHREAD_MUTEX_RECURSIVE,
        // CLOCK_REALTIME, sem_timedwait) required by the shim.
        .define("_GNU_SOURCE", None);

    if !nr_enabled {
        build.define("WDSP_NO_RNNOISE", None);
        build.define("WDSP_NO_SPECBLEACH", None);
    }

    for src in WDSP_C_SOURCES {
        build.file(staged_dir.join(src));
    }
    // Upstream Thetis' `rnnr.c` / `sbnr.c` are compiled when the
    // matching vendored lib was built. When the `nr` feature is off,
    // the stubs in `wdsp_nr_stubs.c` take over — gated internally
    // by `WDSP_NO_RNNOISE` / `WDSP_NO_SPECBLEACH`.
    if nr_enabled {
        build.file(staged_dir.join("rnnr.c"));
        build.file(staged_dir.join("sbnr.c"));
    }
    build.file(shim_dir.join("wdsp_nr_stubs.c"));

    // POSIX pthread glue — only on non-Windows targets. On mingw the
    // real Win32 CRITICAL_SECTION / _beginthread live in w32api so
    // WDSP needs no glue at all.
    if !target_is_windows {
        build.file(shim_dir.join("wdsp_posix.c"));
    }

    build.compile("wdsp");

    // FFTW / rnnoise / libspecbleach link directives are emitted by
    // their respective `build_*` helpers. Host-specific runtime libs
    // go here — note the target_os gating uses `CARGO_CFG_TARGET_OS`
    // (build-script env var) rather than `#[cfg]` attributes so the
    // decision follows the target, not the host the build script
    // runs on.
    if target_os == "linux" {
        println!("cargo:rustc-link-lib=pthread");
        println!("cargo:rustc-link-lib=m");
    } else if target_is_windows {
        // WDSP pulls in avrt (MMCSS thread priority) and winmm
        // (timeBeginPeriod). kernel32 / user32 / ws2_32 come from
        // mingw's default link set.
        println!("cargo:rustc-link-lib=avrt");
        println!("cargo:rustc-link-lib=winmm");
    }
}

/// Build FFTW 3.3.10 from the vendored sources in
/// `crates/wdsp-sys/vendor/fftw-3.3.10/`. Runs cmake twice (double + single
/// precision) because FFTW cannot emit both from a single configure pass.
///
/// Returns the include path that's common to both builds (the FFTW
/// headers are precision-agnostic — a single `fftw3.h` exposes both
/// `fftw_*` and `fftwf_*` symbols). Link search paths for the two
/// static libs are emitted as `cargo:rustc-link-search` directives so
/// downstream crates don't need to know where the libs live.
fn build_fftw(manifest_dir: &Path) -> PathBuf {
    let src_dir = manifest_dir.join("vendor").join("fftw-3.3.10");
    assert!(
        src_dir.is_dir(),
        "FFTW sources not found at {:?}. Did the vendor/ copy get lost?",
        src_dir
    );

    // Tell cargo to rebuild when the vendored source tree changes
    // (typically on upstream bump).
    println!("cargo:rerun-if-changed={}", src_dir.display());

    // Optimisation flags. On x86_64 we enable SSE2 + AVX to match the
    // performance of a system-installed libfftw3. AVX2 is intentionally
    // left off because it can trigger SIGILL on older CPUs and the
    // perf gain is marginal for our FFT sizes (64..262144).
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let enable_sse2 = matches!(target_arch.as_str(), "x86_64" | "x86");
    let enable_avx  = matches!(target_arch.as_str(), "x86_64");

    // --- Double precision ------------------------------------------------
    let mut cfg = cmake::Config::new(&src_dir);
    cfg.define("BUILD_SHARED_LIBS", "OFF")
       .define("BUILD_TESTS", "OFF")
       .define("DISABLE_FORTRAN", "ON")
       .define("ENABLE_FLOAT", "OFF")
       .define("CMAKE_POSITION_INDEPENDENT_CODE", "ON")
       // FFTW 3.3.10 declares `cmake_minimum_required(VERSION 3.0)`
       // but CMake 4 removed compat with everything below 3.5. The
       // 3.3.10 tree actually works fine under 3.5+ semantics — we
       // just need to tell CMake to pretend the minimum is 3.5.
       .define("CMAKE_POLICY_VERSION_MINIMUM", "3.5")
       // mingw-w64 has neither `memalign` nor `posix_memalign`, so
       // FFTW's default aligned-malloc path errors out with a
       // `#error "Don't know how to malloc() aligned memory"`. Turning
       // on `WITH_OUR_MALLOC` uses a small portable pointer-alignment
       // wrapper around plain `malloc` — marginally slower than the
       // native posix_memalign path, but the only one that compiles
       // under mingw.
       //
       // ⚠️ It's NOT a cmake option — FFTW's CMakeLists.txt never
       // reads it. We have to inject it as a raw C preprocessor flag
       // for it to reach `kernel/kalloc.c`. Applied unconditionally
       // (native Linux / macOS) since the perf difference on our FFT
       // sizes is negligible and the code path is identical across
       // targets, which is worth the simplification.
       .cflag("-DWITH_OUR_MALLOC")
       // `cmake` crate sets CMAKE_INSTALL_PREFIX for us. We steer the
       // build to its own sub-dir so the two precision passes don't
       // conflict.
       .out_dir(std::env::var("OUT_DIR").unwrap() + "/fftw-double");
    if enable_sse2 { cfg.define("ENABLE_SSE2", "ON"); }
    if enable_avx  { cfg.define("ENABLE_AVX",  "ON"); }
    let double_dst = cfg.build();
    println!("cargo:rustc-link-search=native={}/lib", double_dst.display());
    // Cross-distro layout: RHEL/Fedora, Arch x86_64 and several CMake
    // hosts put static libs in `lib64/` instead of `lib/`. Emit both
    // search paths so whichever cmake picks at build time is covered.
    println!("cargo:rustc-link-search=native={}/lib64", double_dst.display());
    println!("cargo:rustc-link-lib=static=fftw3");

    // --- Single precision ------------------------------------------------
    let mut cfg = cmake::Config::new(&src_dir);
    cfg.define("BUILD_SHARED_LIBS", "OFF")
       .define("BUILD_TESTS", "OFF")
       .define("DISABLE_FORTRAN", "ON")
       .define("ENABLE_FLOAT", "ON")
       .define("CMAKE_POSITION_INDEPENDENT_CODE", "ON")
       .define("CMAKE_POLICY_VERSION_MINIMUM", "3.5")
       .cflag("-DWITH_OUR_MALLOC")
       .out_dir(std::env::var("OUT_DIR").unwrap() + "/fftw-single");
    if enable_sse2 { cfg.define("ENABLE_SSE2", "ON"); }
    if enable_avx  { cfg.define("ENABLE_AVX",  "ON"); }
    let single_dst = cfg.build();
    println!("cargo:rustc-link-search=native={}/lib", single_dst.display());
    println!("cargo:rustc-link-search=native={}/lib64", single_dst.display());
    println!("cargo:rustc-link-lib=static=fftw3f");

    // The `fftw3.h` header is identical in both install trees (it's
    // precision-agnostic); return the double-precision one.
    double_dst.join("include")
}

/// Copy every `.c` and `.h` file from upstream into `dest`, creating `dest`
/// if necessary and wiping any stale files so a fresh patch sequence always
/// lands on unmodified upstream.
fn stage_upstream(upstream: &Path, dest: &Path) {
    if dest.exists() {
        fs::remove_dir_all(dest)
            .unwrap_or_else(|e| panic!("failed to clear stage dir {:?}: {e}", dest));
    }
    fs::create_dir_all(dest)
        .unwrap_or_else(|e| panic!("failed to create stage dir {:?}: {e}", dest));

    let entries = fs::read_dir(upstream)
        .unwrap_or_else(|e| panic!("failed to read upstream dir {:?}: {e}", upstream));

    for entry in entries {
        let entry = entry.expect("directory entry");
        let path  = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "c" && ext != "h" {
            continue;
        }
        let name = path.file_name().expect("file name");
        fs::copy(&path, dest.join(name))
            .unwrap_or_else(|e| panic!("failed to copy {:?}: {e}", path));
    }
}

/// Build rnnoise (NR3) from the vendored sources in
/// `crates/wdsp-sys/vendor-nr/rnnoise/`. Returns the include path to
/// pass as `-I` to the WDSP compile step (so `rnnr.c` sees the real
/// `rnnoise.h`), or `None` when the `nr` feature is disabled.
///
/// Source list matches upstream rnnoise `Makefile.am:librnnoise_la_SOURCES`
/// at HEAD — 7 files, all self-contained C with an inline baked-in
/// neural-net model in `rnn_data.c`. No external model download
/// required at build time.
fn build_rnnoise(manifest_dir: &Path, enabled: bool) -> Option<PathBuf> {
    if !enabled {
        return None;
    }
    let src_dir = manifest_dir.join("vendor-nr").join("rnnoise");
    assert!(
        src_dir.join("src").join("denoise.c").exists(),
        "rnnoise submodule not initialised at {:?}. \
         Run `git submodule update --init`.",
        src_dir,
    );
    println!("cargo:rerun-if-changed={}", src_dir.display());

    const RNNOISE_SOURCES: &[&str] = &[
        "src/denoise.c",
        "src/rnn.c",
        "src/rnn_data.c",
        "src/rnn_reader.c",
        "src/pitch.c",
        "src/kiss_fft.c",
        "src/celt_lpc.c",
    ];

    let mut build = cc::Build::new();
    build
        .include(src_dir.join("include"))
        .include(src_dir.join("src"))
        .flag_if_supported("-std=c99")
        .flag_if_supported("-fvisibility=hidden")
        .flag_if_supported("-Wno-unused-function")
        .flag_if_supported("-Wno-unused-variable")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-calloc-transposed-args")
        // rnnoise uses `rint` / `lrintf` / `M_PI` from <math.h>.
        // glibc exposes `rint` only with POSIX 200809L, and `M_PI`
        // only with _GNU_SOURCE / _XOPEN_SOURCE. Setting both covers
        // every libc we care about (glibc, musl, mingw's msvcrt).
        .define("_POSIX_C_SOURCE", "200809L")
        .define("_GNU_SOURCE", None)
        .define("_USE_MATH_DEFINES", None);
    for src in RNNOISE_SOURCES {
        build.file(src_dir.join(src));
    }
    build.compile("rnnoise");
    // cc-rs already emits `cargo:rustc-link-lib=static=rnnoise` for us.
    Some(src_dir.join("include"))
}

/// Build libspecbleach (NR4) from the vendored sources in
/// `crates/wdsp-sys/vendor-nr/libspecbleach/`. Returns the include
/// path to pass as `-I` to the WDSP compile step, or `None` when
/// the `nr` feature is disabled.
///
/// libspecbleach internally does its own FFTs via FFTW, so the
/// compile also needs the vendored FFTW include path we just built.
///
/// Source list matches upstream libspecbleach v0.2.0's `src/**/*.c`.
fn build_specbleach(manifest_dir: &Path, enabled: bool, fftw_include: &Path) -> Option<PathBuf> {
    if !enabled {
        return None;
    }
    let src_dir = manifest_dir.join("vendor-nr").join("libspecbleach");
    assert!(
        src_dir.join("include").join("specbleach_adenoiser.h").exists(),
        "libspecbleach submodule not initialised at {:?}. \
         Run `git submodule update --init`.",
        src_dir,
    );
    println!("cargo:rerun-if-changed={}", src_dir.display());

    const SPECBLEACH_SOURCES: &[&str] = &[
        "src/processors/adaptivedenoiser/adaptive_denoiser.c",
        "src/processors/denoiser/spectral_denoiser.c",
        "src/processors/specbleach_adenoiser.c",
        "src/processors/specbleach_denoiser.c",
        "src/shared/gain_estimation/gain_estimators.c",
        "src/shared/noise_estimation/adaptive_noise_estimator.c",
        "src/shared/noise_estimation/noise_estimator.c",
        "src/shared/noise_estimation/noise_profile.c",
        "src/shared/post_estimation/noise_floor_manager.c",
        "src/shared/post_estimation/postfilter.c",
        "src/shared/post_estimation/spectral_whitening.c",
        "src/shared/pre_estimation/absolute_hearing_thresholds.c",
        "src/shared/pre_estimation/critical_bands.c",
        "src/shared/pre_estimation/masking_estimator.c",
        "src/shared/pre_estimation/noise_scaling_criterias.c",
        "src/shared/pre_estimation/spectral_smoother.c",
        "src/shared/pre_estimation/transient_detector.c",
        "src/shared/stft/fft_transform.c",
        "src/shared/stft/stft_buffer.c",
        "src/shared/stft/stft_processor.c",
        "src/shared/stft/stft_windows.c",
        "src/shared/utils/denoise_mixer.c",
        "src/shared/utils/general_utils.c",
        "src/shared/utils/spectral_features.c",
        "src/shared/utils/spectral_trailing_buffer.c",
        "src/shared/utils/spectral_utils.c",
    ];

    let mut build = cc::Build::new();
    build
        .include(src_dir.join("include"))
        // libspecbleach sources do `#include "shared/…"`, relative to
        // the `src/` directory rather than the repo root.
        .include(src_dir.join("src"))
        .include(fftw_include)
        .flag_if_supported("-std=c11")
        .flag_if_supported("-fvisibility=hidden")
        .flag_if_supported("-Wno-unused-function")
        .flag_if_supported("-Wno-unused-variable")
        .flag_if_supported("-Wno-unused-parameter");
    for src in SPECBLEACH_SOURCES {
        build.file(src_dir.join(src));
    }
    build.compile("specbleach");
    Some(src_dir.join("include"))
}

/// Apply every `patches/*.patch` to the staged source tree, in lexicographic
/// order. Aborts the build with a clear error if any patch fails to apply.
fn apply_patches(patches_dir: &Path, staged_dir: &Path) {
    if !patches_dir.exists() {
        return;
    }

    let mut patches: Vec<PathBuf> = fs::read_dir(patches_dir)
        .unwrap_or_else(|e| panic!("failed to read patches dir {:?}: {e}", patches_dir))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("patch"))
        .collect();
    patches.sort();

    for patch in patches {
        let status = Command::new("patch")
            .arg("-p1")
            .arg("--forward")
            .arg("--silent")
            .arg("-i")
            .arg(&patch)
            .current_dir(staged_dir)
            .status()
            .unwrap_or_else(|e| panic!("failed to invoke `patch`: {e}. Install GNU patch."));
        if !status.success() {
            panic!("patch {:?} failed to apply cleanly to {:?}", patch, staged_dir);
        }
    }
}
