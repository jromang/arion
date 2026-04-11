//! Build script for `wdsp-sys`.
//!
//! Pipeline:
//! 1. Locate upstream WDSP sources in the `thetis-upstream/` submodule.
//! 2. Stage a fresh copy into `$OUT_DIR/wdsp/` on every build (cheap: ~140
//!    small text files, and only when any of them changed upstream or any
//!    patch changed).
//! 3. Apply every `patches/*.patch` in lexicographic order via `patch -p1`.
//! 4. Compile the staged sources with `cc`, intercepting Windows-only includes
//!    (`<Windows.h>`, `<process.h>`, `<intrin.h>`, `<avrt.h>`) with the stub
//!    headers under `shim/`.
//! 5. Link FFTW3 (single + double precision) via pkg-config.
//!
//! Why patches instead of a vendored fork: almost every portability change
//! (win32 → pthread, atomics, `_aligned_malloc`, etc.) lives in `shim/` and
//! never touches upstream. Only the handful of true source fixes (bugs that
//! were latent under MSVC but rejected by gcc/clang) belong here, and keeping
//! them as unified diffs makes every modification explicit and reviewable.

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

    let shim_dir    = manifest_dir.join("shim");
    let patches_dir = manifest_dir.join("patches");
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

    // --- FFTW3 (both precisions) ----------------------------------------
    // Upstream uses `fftwf_*` (single) and `fftw_*` (double) — notably emnr
    // and cfcomp run double-precision overlap-save FFTs. Link both.
    let fftw3f = pkg_config::Config::new()
        .atleast_version("3.3")
        .probe("fftw3f")
        .expect("fftw3f (libfftw3f-dev) is required to build wdsp-sys");
    let _fftw3 = pkg_config::Config::new()
        .atleast_version("3.3")
        .probe("fftw3")
        .expect("fftw3 (libfftw3-dev) is required to build wdsp-sys");

    // --- Optional NR libraries -----------------------------------------
    //
    // `rnnoise` (NR3) and `libspecbleach` (NR4) are built by their
    // upstream distros as standard shared libraries; we use them via
    // `pkg-config` when they're present and fall back to the
    // `shim/wdsp_nr_stubs.c` no-ops when they aren't. This keeps
    // phase A's zero-dependency build still working while letting
    // phase B users with `rnnoise` / `libspecbleach` installed get
    // the real feature.
    //
    // Flipping a lib on/off controls two things simultaneously:
    //   1. `WDSP_NO_RNNOISE` / `WDSP_NO_SPECBLEACH` defines — WDSP's
    //      own `rnnr.c` / `sbnr.c` and the stubs in `wdsp_nr_stubs.c`
    //      are both gated on them.
    //   2. Whether `rnnr.c` / `sbnr.c` are compiled at all. When the
    //      lib is absent, the stubs provide every symbol the rest of
    //      WDSP links against.
    let rnnoise = pkg_config::Config::new().probe("rnnoise").ok();
    let specbleach = pkg_config::Config::new().probe("specbleach").ok();

    // --- Stage a fresh copy of upstream sources into OUT_DIR ------------
    let staged_dir = out_dir.join("wdsp");
    stage_upstream(&upstream_wdsp, &staged_dir);

    // --- Apply patches in lexicographic order ---------------------------
    apply_patches(&patches_dir, &staged_dir);

    // --- Cargo rerun triggers -------------------------------------------
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", shim_dir.display());
    println!("cargo:rerun-if-changed={}", patches_dir.display());
    println!("cargo:rerun-if-changed={}", upstream_wdsp.display());

    // --- Compile --------------------------------------------------------
    //
    // Include-path ordering matters:
    //   1. Optional NR include dirs first, so real `rnnoise.h` /
    //      `specbleach_adenoiser.h` win over the fallback stubs in
    //      `shim/`.
    //   2. `shim/` second, to intercept `<Windows.h>` / `<process.h>`
    //      / `<intrin.h>` / `<avrt.h>` (the host toolchain has none
    //      of those, so this is a safe fall-through).
    //   3. The staged WDSP source dir last.
    let mut build = cc::Build::new();
    if let Some(info) = &rnnoise {
        for inc in &info.include_paths {
            build.include(inc);
        }
    }
    if let Some(info) = &specbleach {
        for inc in &info.include_paths {
            build.include(inc);
        }
    }
    // NR fallback headers live in `shim/nr-stub/<lib>/` — one sub-dir
    // per optional lib. We add a sub-dir to the `-I` chain *only* when
    // the matching lib is missing from pkg-config, so the real header
    // (typically under `/usr/include`) wins everywhere else. Keeping
    // each stub in its own directory avoids the "one missing lib drags
    // in everyone's stub" trap: if `rnnoise` is installed but
    // `libspecbleach` isn't, we add only the specbleach stub path, so
    // the real `rnnoise.h` stays visible.
    if rnnoise.is_none() {
        build.include(shim_dir.join("nr-stub").join("rnnoise"));
    }
    if specbleach.is_none() {
        build.include(shim_dir.join("nr-stub").join("specbleach"));
    }
    build
        .include(&shim_dir)
        .include(&staged_dir)
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

    if rnnoise.is_none() {
        build.define("WDSP_NO_RNNOISE", None);
    }
    if specbleach.is_none() {
        build.define("WDSP_NO_SPECBLEACH", None);
    }

    for inc in &fftw3f.include_paths {
        build.include(inc);
    }

    for src in WDSP_C_SOURCES {
        build.file(staged_dir.join(src));
    }
    // Re-enable `rnnr.c` / `sbnr.c` when the matching NR lib is available.
    if rnnoise.is_some() {
        build.file(staged_dir.join("rnnr.c"));
        tracing_emit("rnnoise detected: NR3 enabled");
    }
    if specbleach.is_some() {
        build.file(staged_dir.join("sbnr.c"));
        tracing_emit("libspecbleach detected: NR4 enabled");
    }

    // NR stubs — each half is guarded internally by `WDSP_NO_RNNOISE`
    // / `WDSP_NO_SPECBLEACH`, so a partial build (one lib present, one
    // missing) gets exactly the right set of fallback symbols.
    build.file(shim_dir.join("wdsp_nr_stubs.c"));

    // POSIX glue — only on non-Windows.
    #[cfg(not(target_os = "windows"))]
    {
        build.file(shim_dir.join("wdsp_posix.c"));
    }

    build.compile("wdsp");

    // pkg-config already emits `cargo:rustc-link-lib=fftw3f` / `fftw3`
    // / `rnnoise` / `specbleach` for the probes that succeeded — we
    // don't need to re-emit them here.
    #[cfg(target_os = "linux")]
    {
        println!("cargo:rustc-link-lib=pthread");
        println!("cargo:rustc-link-lib=m");
    }
}

/// Log a build-script message that shows up in `cargo build -vv`.
fn tracing_emit(msg: &str) {
    println!("cargo:warning={}", msg);
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
