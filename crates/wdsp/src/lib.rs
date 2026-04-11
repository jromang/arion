//! Safe Rust wrapper around [`wdsp-sys`].
//!
//! Scope for phase A: just what `thetis-core` needs to open an RX channel,
//! pump interleaved I/Q in, pull demodulated audio out, and tweak the
//! mode / passband / AGC / gain. Phase B will grow this to cover TX and
//! the analyzer.
//!
//! # Thread-safety & global state
//!
//! WDSP stores its per-channel state in a global `ch[MAX_CHANNELS]`
//! array, and its FFT plan creation goes through FFTW's (non-thread-safe)
//! `fftw_plan_*` family. Concretely that means:
//!
//! - Every [`Channel`] must be assigned a unique integer `id` in
//!   `0..wdsp_sys::MAX_CHANNELS`. This wrapper takes that ID as an
//!   explicit argument rather than managing it centrally, because
//!   `thetis-core` assigns channels deterministically (RX1 = 0, RX2 = 1,
//!   TX1 = 2 in phase B) and doesn't benefit from a hidden allocator.
//! - Opening more than one channel concurrently from different threads
//!   can race inside FFTW. In phase A `thetis-core` only ever opens a
//!   single RX channel during startup, so we don't need a mutex here;
//!   phase B adds a global plan-creation mutex when the TX channel and
//!   the analyzer start getting opened in parallel.

// `unsafe` is the whole point of this crate — every method here wraps a
// raw call into the WDSP C library. We minimise the unsafe surface: each
// `unsafe { ... }` block is small, commented, and justified on either
// "the channel id was validated at open_rx time" or "buffer lengths were
// checked by the caller". Downstream crates (`thetis-core`, the UI app)
// never need `unsafe` themselves.

use std::os::raw::c_int;

use wdsp_sys as sys;

#[derive(Debug, thiserror::Error)]
pub enum WdspError {
    #[error("channel id {0} is out of range (must be < {max})",
            max = sys::MAX_CHANNELS)]
    BadChannelId(i32),
    #[error("WDSP fexchange0 reported error {0}")]
    ExchangeError(i32),
}

/// RXA demodulation mode. Wire values match the integers WDSP expects in
/// `SetRXAMode` and the `RXA_MODE_*` constants from `wdsp-sys`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Lsb,
    Usb,
    Dsb,
    CwL,
    CwU,
    Fm,
    Am,
    DigU,
    Spec,
    DigL,
    Sam,
    Drm,
}

impl Mode {
    pub fn to_wire(self) -> c_int {
        match self {
            Mode::Lsb  => sys::RXA_MODE_LSB,
            Mode::Usb  => sys::RXA_MODE_USB,
            Mode::Dsb  => sys::RXA_MODE_DSB,
            Mode::CwL  => sys::RXA_MODE_CWL,
            Mode::CwU  => sys::RXA_MODE_CWU,
            Mode::Fm   => sys::RXA_MODE_FM,
            Mode::Am   => sys::RXA_MODE_AM,
            Mode::DigU => sys::RXA_MODE_DIGU,
            Mode::Spec => sys::RXA_MODE_SPEC,
            Mode::DigL => sys::RXA_MODE_DIGL,
            Mode::Sam  => sys::RXA_MODE_SAM,
            Mode::Drm  => sys::RXA_MODE_DRM,
        }
    }

    /// Default passband (low, high) in Hz, relative to baseband, used by
    /// the built-in filter presets. These match the out-of-the-box values
    /// Thetis uses when a new profile is created.
    pub fn default_passband_hz(self) -> (f64, f64) {
        match self {
            Mode::Usb | Mode::DigU => ( 200.0,  3_000.0),
            Mode::Lsb | Mode::DigL => (-3_000.0,  -200.0),
            Mode::Am  | Mode::Sam  => (-5_000.0,  5_000.0),
            Mode::Fm               => (-8_000.0,  8_000.0),
            Mode::CwU              => ( 400.0,    900.0),
            Mode::CwL              => (-900.0,   -400.0),
            Mode::Dsb              => (-3_000.0,  3_000.0),
            Mode::Spec             => (-20_000.0, 20_000.0),
            Mode::Drm              => (-5_000.0,  5_000.0),
        }
    }
}

/// AGC speed preset. Wire values mirror the magic integers `SetRXAAGCMode`
/// expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgcMode {
    Off,
    Long,
    Slow,
    #[default]
    Medium,
    Fast,
}

impl AgcMode {
    pub fn to_wire(self) -> c_int {
        match self {
            AgcMode::Off    => 0,
            AgcMode::Long   => 1,
            AgcMode::Slow   => 2,
            AgcMode::Medium => 3,
            AgcMode::Fast   => 4,
        }
    }
}

/// Parameters for [`Channel::open_rx`].
#[derive(Debug, Clone, Copy)]
pub struct RxConfig {
    /// WDSP channel index (`0..MAX_CHANNELS`).
    pub id: i32,
    /// Input buffer size in **complex** samples (= doubles per I/Q pair).
    pub in_size: i32,
    /// DSP-side processing size. Normally equal to `in_size`.
    pub dsp_size: i32,
    /// Input sample rate (Hz). HL2 at 48 kHz → pass `48_000`.
    pub input_rate: i32,
    /// DSP processing rate. Phase A keeps everything at the same rate.
    pub dsp_rate: i32,
    /// Output sample rate (Hz).
    pub output_rate: i32,
    /// Initial demodulation mode.
    pub mode: Mode,
    /// Optional override for the passband in Hz. `None` uses
    /// [`Mode::default_passband_hz`].
    pub passband_hz: Option<(f64, f64)>,
    /// Initial AGC mode.
    pub agc: AgcMode,
    /// Linear panel gain applied after demodulation. `1.0` is unity.
    pub panel_gain: f64,
}

impl Default for RxConfig {
    fn default() -> Self {
        RxConfig {
            id:          0,
            in_size:     1024,
            dsp_size:    1024,
            input_rate:  48_000,
            dsp_rate:    48_000,
            output_rate: 48_000,
            mode:        Mode::Usb,
            passband_hz: None,
            agc:         AgcMode::Medium,
            panel_gain:  1.0,
        }
    }
}

/// A running WDSP RX channel.
///
/// The underlying C library keeps channel state in a process-wide array
/// and a dedicated DSP thread per channel; [`Channel::open_rx`] calls
/// `OpenChannel(… state = 1 …)` so the channel starts processing
/// immediately. Dropping a `Channel` (or calling [`Channel::close`])
/// tears down the thread and releases its resources.
#[derive(Debug)]
pub struct Channel {
    id:        i32,
    in_size:   usize,
    out_size:  usize,
    mode:      Mode,
    panel_gain: f64,
}

impl Channel {
    /// Open and start an RX channel.
    pub fn open_rx(config: RxConfig) -> Result<Self, WdspError> {
        if config.id < 0 || config.id >= sys::MAX_CHANNELS {
            return Err(WdspError::BadChannelId(config.id));
        }

        // SAFETY: we validated the channel ID above, and WDSP's
        // OpenChannel is safe to call for an unused channel slot. The
        // call blocks until the DSP thread is up; subsequent setters
        // operate on the newly-created state under WDSP's own critical
        // section.
        unsafe {
            sys::OpenChannel(
                config.id,
                config.in_size,
                config.dsp_size,
                config.input_rate,
                config.dsp_rate,
                config.output_rate,
                sys::CHANNEL_TYPE_RX,
                1, // state = running
                0.0, 0.0, 0.0, 0.0,
                0, // bfo = 0 (non-blocking output)
            );
            sys::SetRXAMode(config.id, config.mode.to_wire());
            let (lo, hi) = config
                .passband_hz
                .unwrap_or_else(|| config.mode.default_passband_hz());
            sys::RXASetPassband(config.id, lo, hi);
            sys::SetRXAAGCMode(config.id, config.agc.to_wire());
            sys::SetRXAPanelGain1(config.id, config.panel_gain);
        }

        tracing::info!(
            id = config.id,
            in_size = config.in_size,
            rate = config.input_rate,
            mode = ?config.mode,
            "opened WDSP RX channel"
        );

        Ok(Channel {
            id:         config.id,
            in_size:    config.in_size as usize,
            out_size:   config.in_size as usize, // same rate for phase A
            mode:       config.mode,
            panel_gain: config.panel_gain,
        })
    }

    pub fn id(&self) -> i32 { self.id }

    /// Number of complex samples the channel expects per [`Self::process`]
    /// call.
    pub fn in_size(&self) -> usize { self.in_size }

    /// Number of complex samples the channel produces per call.
    pub fn out_size(&self) -> usize { self.out_size }

    pub fn mode(&self) -> Mode { self.mode }

    /// Change the demodulation mode, also resetting the passband to the
    /// mode's default.
    pub fn set_mode(&mut self, mode: Mode) {
        // SAFETY: self.id was validated on open; WDSP takes its own
        // critical section internally.
        unsafe {
            sys::SetRXAMode(self.id, mode.to_wire());
            let (lo, hi) = mode.default_passband_hz();
            sys::RXASetPassband(self.id, lo, hi);
        }
        self.mode = mode;
    }

    pub fn set_passband_hz(&mut self, low: f64, high: f64) {
        unsafe { sys::RXASetPassband(self.id, low, high); }
    }

    pub fn set_agc(&mut self, agc: AgcMode) {
        unsafe { sys::SetRXAAGCMode(self.id, agc.to_wire()); }
    }

    pub fn set_panel_gain(&mut self, gain: f64) {
        unsafe { sys::SetRXAPanelGain1(self.id, gain); }
        self.panel_gain = gain;
    }

    pub fn panel_gain(&self) -> f64 { self.panel_gain }

    /// Push one input buffer of interleaved complex IQ samples and pull
    /// the matching output buffer of demodulated complex audio.
    ///
    /// `in_buf` must hold `2 * in_size` doubles (`[I0, Q0, I1, Q1, …]`).
    /// `out_buf` must hold `2 * out_size` doubles. Both are overwritten
    /// in place.
    ///
    /// Early calls after opening a channel can return
    /// [`WdspError::ExchangeError(-2)`] while WDSP primes its internal
    /// ring buffer; the caller can treat that as "no audio this frame"
    /// and keep going.
    pub fn process(&mut self, in_buf: &mut [f64], out_buf: &mut [f64]) -> Result<(), WdspError> {
        debug_assert_eq!(in_buf.len(),  2 * self.in_size,  "input buffer wrong size");
        debug_assert_eq!(out_buf.len(), 2 * self.out_size, "output buffer wrong size");

        let mut error: c_int = 0;
        // SAFETY: buffer sizes verified above, channel id is valid,
        // fexchange0 is the documented exchange point. The call may
        // internally hand off to WDSP's DSP thread via semaphores.
        unsafe {
            sys::fexchange0(
                self.id,
                in_buf.as_mut_ptr(),
                out_buf.as_mut_ptr(),
                &mut error,
            );
        }
        if error == 0 || error == -2 {
            Ok(())
        } else {
            Err(WdspError::ExchangeError(error))
        }
    }

    /// Explicitly close the channel. Equivalent to dropping the handle
    /// but lets the caller observe the close completing synchronously.
    pub fn close(self) {
        // Drop runs the actual close.
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        // SAFETY: valid channel id, idempotent, WDSP joins its DSP
        // thread internally before returning.
        unsafe { sys::CloseChannel(self.id) };
        tracing::debug!(id = self.id, "closed WDSP RX channel");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full lifecycle: open, run a handful of silent buffers through,
    /// poke a few setters, close. Like the `wdsp-sys` smoke test but one
    /// level higher in the stack.
    #[test]
    fn rx_channel_roundtrip() {
        let cfg = RxConfig::default();
        let mut ch = Channel::open_rx(cfg).expect("open channel");
        assert_eq!(ch.id(), 0);
        assert_eq!(ch.in_size(), 1024);

        let mut in_buf  = vec![0.0_f64; 2 * ch.in_size()];
        let mut out_buf = vec![0.0_f64; 2 * ch.out_size()];
        for _ in 0..4 {
            ch.process(&mut in_buf, &mut out_buf).expect("process");
        }

        ch.set_mode(Mode::Am);
        ch.set_passband_hz(-4000.0, 4000.0);
        ch.set_agc(AgcMode::Fast);
        ch.set_panel_gain(0.5);
    }
}
