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
//! equivalent on macOS / Windows via the `directories` crate). First run
//! still pays the rebuild cost — and in fact takes *longer* than the
//! lazy per-channel path because upstream primes every size up to
//! 262144 — but every subsequent run is instantaneous.

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
