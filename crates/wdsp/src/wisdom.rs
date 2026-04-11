//! FFTW plan cache ("wisdom") management.
//!
//! FFTW builds execution plans for every FFT size it encounters at
//! runtime, and building a plan at `FFTW_PATIENT` — which is what WDSP
//! uses internally — takes several seconds for each size. The first
//! [`wdsp::Channel::open_rx`] in a fresh process pays that cost for
//! every size the RX chain needs, which is why phase A's Radio startup
//! takes ~3 seconds.
//!
//! FFTW serialises plans to a "wisdom file" that can be re-imported on
//! the next run, skipping the planning phase entirely. Upstream
//! [`wisdom.c`] already wraps this in a single-call `WDSPwisdom(dir)`
//! entry point that either imports an existing `<dir>wdspWisdom00` or
//! rebuilds the full plan table.
//!
//! We stash the file under `$XDG_CACHE_HOME/thetis/` (platform-appropriate
//! equivalent on macOS / Windows via the `directories` crate). Subsequent
//! runs are instantaneous thanks to that cache.
//!
//! To avoid the 1–3 minute rebuild on the **very first** launch (new
//! machine, fresh install), we also bake a pre-built wisdom file into
//! the binary via `include_bytes!`. [`seed_cache_with_embedded`] writes
//! the blob to the cache directory iff no user-local file exists, so
//! that the subsequent [`WDSPwisdom`] call imports it instantly.
//! The blob is regenerated with the `gen_wisdom` example whenever
//! FFTW or the WDSP plan sizes change.

use std::ffi::CString;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use wdsp_sys as sys;

/// Outcome of a [`prime`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WisdomStatus {
    /// The cache file existed and was imported without modification.
    Loaded,
    /// The cache file was missing or unusable and FFTW had to rebuild
    /// the whole plan table. This may take 30+ seconds.
    Rebuilt,
}

/// Errors returned when priming the wisdom cache.
#[derive(Debug, thiserror::Error)]
pub enum WisdomError {
    #[error("could not determine a cache directory for this platform")]
    NoCacheDir,

    #[error("failed to create cache directory {path:?}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("cache path contains a NUL byte: {0:?}")]
    InteriorNul(PathBuf),
}

/// Return the default wisdom cache directory,
/// `$XDG_CACHE_HOME/thetis/` on Linux, `~/Library/Caches/thetis/` on
/// macOS, `%LOCALAPPDATA%\thetis\cache\` on Windows.
///
/// Returns `None` only if the OS refuses to hand out a home-dir —
/// typical on headless CI containers where `HOME` is unset.
pub fn default_cache_dir() -> Option<PathBuf> {
    ProjectDirs::from("rs", "thetis", "thetis").map(|p| p.cache_dir().to_path_buf())
}

/// Prime FFTW's wisdom cache. Call this **once**, before opening any
/// [`Channel`](crate::Channel).
///
/// - Creates `dir` if it doesn't exist.
/// - Calls [`wdsp_sys::WDSPwisdom`] which imports `<dir>wdspWisdom00` if
///   present or rebuilds the full plan table if not.
/// - Returns [`WisdomStatus::Loaded`] on cache hit, [`WisdomStatus::Rebuilt`]
///   on miss.
///
/// If anything goes wrong this function is safe to ignore — priming is
/// a pure optimisation and a panic here would make us worse off than
/// phase A.
pub fn prime<P: AsRef<Path>>(dir: P) -> Result<WisdomStatus, WisdomError> {
    let dir_buf = dir.as_ref().to_path_buf();

    // Ensure the directory exists.
    std::fs::create_dir_all(&dir_buf).map_err(|e| WisdomError::CreateDir {
        path: dir_buf.clone(),
        source: e,
    })?;

    // Upstream `WDSPwisdom` does:
    //     strcpy(wisdom_file, directory);
    //     strcat(wisdom_file, "wdspWisdom00");
    // so we MUST hand it a directory that ends in a path separator,
    // otherwise it would try to open `/cache/thetiswdspWisdom00`.
    let mut with_sep = dir_buf.clone().into_os_string();
    with_sep.push(std::path::MAIN_SEPARATOR.to_string());

    // `into_string` rejects non-UTF8 paths — a restriction we accept
    // because FFTW's own filename handling is `char*`-based anyway.
    let dir_string = with_sep
        .into_string()
        .map_err(|_| WisdomError::InteriorNul(dir_buf.clone()))?;
    let c_dir = CString::new(dir_string)
        .map_err(|_| WisdomError::InteriorNul(dir_buf.clone()))?;

    tracing::info!(dir = %c_dir.to_string_lossy(), "priming FFTW wisdom");

    // SAFETY: `WDSPwisdom` reads a NUL-terminated string, which
    // `CString` guarantees. It writes only to the FFTW global plan
    // cache (protected internally), not to our buffer.
    let rc = unsafe { sys::WDSPwisdom(c_dir.as_ptr()) };

    let status = if rc == 0 {
        WisdomStatus::Loaded
    } else {
        WisdomStatus::Rebuilt
    };
    tracing::info!(?status, "wisdom primed");
    Ok(status)
}

/// Convenience: prime the wisdom cache at the default platform
/// directory. Returns `Ok(None)` if no cache dir can be determined
/// (headless CI), rather than an error.
pub fn prime_default() -> Result<Option<WisdomStatus>, WisdomError> {
    match default_cache_dir() {
        Some(dir) => prime(&dir).map(Some),
        None => {
            tracing::warn!("no cache directory available, skipping wisdom prime");
            Ok(None)
        }
    }
}

/// Pre-built FFTW wisdom blob, embedded into the binary at compile
/// time. Generated by `cargo run -p wdsp --example gen_wisdom` against
/// the vendored FFTW build, on an x86_64 host with SSE2 + AVX. Loading
/// this on a CPU without AVX still works — `fftw_import_wisdom` skips
/// any plan whose codelet symbol isn't compiled into the running
/// binary, and `WDSPwisdom` then falls back to building those few
/// missing sizes from scratch.
pub const EMBEDDED_WISDOM: &[u8] = include_bytes!("../data/wdspWisdom00");

/// Seed the cache directory with the embedded wisdom blob if there
/// isn't already a user-local cache file. This is the path that
/// makes a fresh-install first launch instantaneous instead of
/// 1–10 minutes of FFTW planning.
///
/// Behaviour:
/// - cache file already present → returns `Ok(false)`, leaves it alone
///   (a previous run may have grown the wisdom with extra plans —
///   never overwrite that).
/// - cache file missing → writes the embedded blob atomically and
///   returns `Ok(true)`.
/// - any I/O failure is bubbled up; `prime_with_embedded_default`
///   logs and continues so a corrupted home dir doesn't make us
///   worse off than the no-embed path.
pub fn seed_cache_with_embedded<P: AsRef<Path>>(dir: P) -> std::io::Result<bool> {
    let dir = dir.as_ref();
    std::fs::create_dir_all(dir)?;
    let target = dir.join("wdspWisdom00");
    if target.exists() {
        return Ok(false);
    }

    // Atomic write so a SIGKILL between create and rename can't
    // leave a half-written wisdom file that WDSPwisdom would treat
    // as corrupt and fall back to a slow rebuild.
    let tmp = dir.join(".wdspWisdom00.tmp");
    std::fs::write(&tmp, EMBEDDED_WISDOM)?;
    std::fs::rename(&tmp, &target)?;
    tracing::info!(
        path = %target.display(),
        bytes = EMBEDDED_WISDOM.len(),
        "seeded wisdom cache from embedded blob"
    );
    Ok(true)
}

/// One-shot startup helper: seed from the embedded blob if needed,
/// then prime FFTW from the resulting cache file. This is the function
/// `thetis-core` calls during `Radio::start`.
///
/// Returns `Ok(None)` on platforms with no cache dir (headless CI),
/// matching `prime_default`. Errors from the seed step are logged
/// and treated as non-fatal — we still try to call `prime` so a
/// read-only `$XDG_CACHE_HOME` doesn't break the radio entirely
/// (FFTW will simply rebuild in-memory plans, like before B.0.7).
pub fn prime_with_embedded_default() -> Result<Option<WisdomStatus>, WisdomError> {
    let Some(dir) = default_cache_dir() else {
        tracing::warn!("no cache directory available, skipping wisdom prime");
        return Ok(None);
    };

    match seed_cache_with_embedded(&dir) {
        Ok(true) => tracing::info!("wisdom cache seeded from embedded blob"),
        Ok(false) => tracing::debug!("wisdom cache already present, leaving as-is"),
        Err(e) => tracing::warn!(error = %e, "failed to seed wisdom cache, falling back to rebuild"),
    }

    prime(&dir).map(Some)
}
