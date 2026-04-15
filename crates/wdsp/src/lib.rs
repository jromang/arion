//! Safe Rust wrapper around [`wdsp-sys`].
//!
//! Scope for phase A: just what `arion-core` needs to open an RX channel,
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
//!   `arion-core` assigns channels deterministically (RX1 = 0, RX2 = 1,
//!   TX1 = 2 in phase B) and doesn't benefit from a hidden allocator.
//! - Opening more than one channel concurrently from different threads
//!   can race inside FFTW. In phase A `arion-core` only ever opens a
//!   single RX channel during startup, so we don't need a mutex here;
//!   phase B adds a global plan-creation mutex when the TX channel and
//!   the analyzer start getting opened in parallel.

// `unsafe` is the whole point of this crate — every method here wraps a
// raw call into the WDSP C library. We minimise the unsafe surface: each
// `unsafe { ... }` block is small, commented, and justified on either
// "the channel id was validated at open_rx time" or "buffer lengths were
// checked by the caller". Downstream crates (`arion-core`, the UI app)
// never need `unsafe` themselves.

pub mod wisdom;
pub use wisdom::{
    default_cache_dir, prime as prime_wisdom, prime_default as prime_wisdom_default,
    prime_with_embedded_default as prime_wisdom_with_embedded_default, WisdomError, WisdomStatus,
};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// Arion uses when a new profile is created.
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

            // Attach the external NB (ANB) and NB2 (NOB) pipelines so
            // they're ready to be toggled on at runtime. Both start off
            // (`run = 0`) — the wrappers short-circuit when disabled,
            // so we can unconditionally call them from `process()`.
            let rate = config.input_rate as f64;
            let buffsize = config.in_size;
            sys::create_anbEXT(
                config.id, 0, buffsize, rate,
                0.000_1,  // tau (100 µs transition)
                0.001,    // hangtime (1 ms at zero)
                0.000_1,  // advtime
                0.02,     // backtau (20 ms averaging)
                30.0,     // threshold multiplier
            );
            sys::create_nobEXT(
                config.id, 0, 0, buffsize, rate,
                0.000_1,
                0.001,
                0.000_1,
                0.02,
                30.0,
            );
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

    /// Enable or disable the RNNoise (NR3) denoiser on this channel.
    ///
    /// Requires `wdsp-sys` to have been built against a real `rnnoise`
    /// system library; otherwise the call is a no-op (handled inside
    /// the stub implementation). No way to query the build-time
    /// availability at runtime — the caller should treat this as
    /// best-effort.
    pub fn set_nr3_enabled(&mut self, enabled: bool) {
        // SAFETY: valid channel id, WDSP serialises with its own csDSP.
        unsafe { sys::SetRXARNNRRun(self.id, if enabled { 1 } else { 0 }); }
    }

    /// Enable or disable the libspecbleach (NR4) adaptive denoiser.
    /// Same best-effort semantics as [`Self::set_nr3_enabled`].
    pub fn set_nr4_enabled(&mut self, enabled: bool) {
        unsafe { sys::SetRXASBNRRun(self.id, if enabled { 1 } else { 0 }); }
    }

    /// NR4 reduction strength in dB (0..=40, upstream default 10).
    pub fn set_nr4_reduction_db(&mut self, db: f32) {
        unsafe { sys::SetRXASBNRreductionAmount(self.id, db); }
    }

    // --- TX DSP controls (used when channel is opened as TX) ----------

    /// Enable/disable the TX frequency-domain compander (CFCOMP).
    pub fn set_tx_cfcomp_run(&mut self, enabled: bool) {
        unsafe { sys::SetTXACFCOMPRun(self.id, i32::from(enabled)); }
    }

    /// Set the TX pre-compression gain in dB.
    pub fn set_tx_cfcomp_precomp(&mut self, db: f64) {
        unsafe { sys::SetTXACFCOMPPrecomp(self.id, db); }
    }

    /// Enable/disable the TX graphic equalizer.
    pub fn set_tx_eq_enabled(&mut self, enabled: bool) {
        unsafe { sys::SetTXAEQRun(self.id, i32::from(enabled)); }
    }

    /// Apply a 10-band TX graphic EQ.
    pub fn set_tx_eq_bands(&mut self, gains: &[i32; 11]) {
        unsafe { sys::SetTXAGrphEQ10(self.id, gains.as_ptr()); }
    }

    /// Set TX panel gain (mic gain, linear scale).
    pub fn set_tx_panel_gain(&mut self, gain: f64) {
        unsafe { sys::SetTXAPanelGain1(self.id, gain); }
    }

    // --- RX DSP controls ------------------------------------------------

    /// Enable or disable the RX graphic equalizer.
    pub fn set_eq_enabled(&mut self, enabled: bool) {
        unsafe { sys::SetRXAEQRun(self.id, i32::from(enabled)); }
    }

    /// Apply a 10-band graphic EQ. `gains` must be exactly 11 values:
    /// `[0]` = preamp gain (dB), `[1..=10]` = band gains at
    /// 32, 63, 125, 250, 500, 1k, 2k, 4k, 8k, 16k Hz.
    pub fn set_eq_bands(&mut self, gains: &[i32; 11]) {
        unsafe { sys::SetRXAGrphEQ10(self.id, gains.as_ptr()); }
    }

    /// Enable or disable the LMS auto-notch filter (ANF).
    pub fn set_anf_enabled(&mut self, enabled: bool) {
        unsafe { sys::SetRXAANFRun(self.id, i32::from(enabled)); }
    }

    /// Enable or disable the spectral noise blanker (SNB).
    pub fn set_snba_enabled(&mut self, enabled: bool) {
        unsafe { sys::SetRXASNBARun(self.id, i32::from(enabled)); }
    }

    /// Enable or disable EMNR (enhanced spectral noise reduction,
    /// a.k.a. "NR2" in Thetis).
    pub fn set_emnr_enabled(&mut self, enabled: bool) {
        unsafe { sys::SetRXAEMNRRun(self.id, i32::from(enabled)); }
    }

    /// Enable or disable the LMS adaptive noise reducer (Thetis "NR").
    pub fn set_anr_enabled(&mut self, enabled: bool) {
        unsafe { sys::SetRXAANRRun(self.id, i32::from(enabled)); }
    }

    // --- Squelch (mode-aware) -----------------------------------------

    /// Toggle the squelch appropriate for the current demod mode. FM
    /// uses the FM amplitude-discriminator squelch, AM uses the AM
    /// level squelch, every other mode (SSB / CW / DIG) uses SSQL.
    pub fn set_squelch_enabled(&mut self, mode: Mode, enabled: bool) {
        let run = i32::from(enabled);
        unsafe {
            match mode {
                Mode::Fm => sys::SetRXAFMSQRun(self.id, run),
                Mode::Am | Mode::Sam => sys::SetRXAAMSQRun(self.id, run),
                _ => sys::SetRXASSQLRun(self.id, run),
            }
        }
    }

    /// Set the squelch threshold. Units depend on the squelch flavour:
    /// FM uses a 0..1 level, AM and SSB/CW use dB (typically -40..0).
    pub fn set_squelch_threshold(&mut self, mode: Mode, threshold: f64) {
        unsafe {
            match mode {
                Mode::Fm => sys::SetRXAFMSQThreshold(self.id, threshold),
                Mode::Am | Mode::Sam => sys::SetRXAAMSQThreshold(self.id, threshold),
                _ => sys::SetRXASSQLThreshold(self.id, threshold),
            }
        }
    }

    // --- APF (CW peak filter) ------------------------------------------

    pub fn set_apf_enabled(&mut self, enabled: bool) {
        unsafe { sys::SetRXASPCWRun(self.id, i32::from(enabled)); }
    }
    pub fn set_apf_freq(&mut self, hz: f64) {
        unsafe { sys::SetRXASPCWFreq(self.id, hz); }
    }
    pub fn set_apf_bandwidth(&mut self, hz: f64) {
        unsafe { sys::SetRXASPCWBandwidth(self.id, hz); }
    }
    pub fn set_apf_gain(&mut self, gain_db: f64) {
        unsafe { sys::SetRXASPCWGain(self.id, gain_db); }
    }

    // --- AGC fine controls ---------------------------------------------

    /// Set the AGC max-gain ceiling, in dB. WDSP computes
    /// `max_gain = 10^(db/20)`, so this is *not* a dBm output cap
    /// despite the upstream `SetRXAAGCTop` name. Sane range 60..120.
    pub fn set_agc_max_gain(&mut self, db: f64) {
        unsafe { sys::SetRXAAGCTop(self.id, db); }
    }
    pub fn set_agc_hang_enabled(&mut self, on: bool) {
        unsafe { sys::SetRXAAGCHang(self.id, i32::from(on)); }
    }
    pub fn set_agc_hang_level(&mut self, level: f64) {
        unsafe { sys::SetRXAAGCHangLevel(self.id, level); }
    }
    pub fn set_agc_hang_threshold(&mut self, threshold: i32) {
        unsafe { sys::SetRXAAGCHangThreshold(self.id, threshold); }
    }
    pub fn set_agc_decay(&mut self, decay_ms: i32) {
        unsafe { sys::SetRXAAGCDecay(self.id, decay_ms); }
    }
    pub fn set_agc_slope(&mut self, slope: i32) {
        unsafe { sys::SetRXAAGCSlope(self.id, slope); }
    }
    pub fn set_agc_fixed_gain(&mut self, gain_db: f64) {
        unsafe { sys::SetRXAAGCFixed(self.id, gain_db); }
    }
    pub fn set_agc_attack(&mut self, attack_ms: i32) {
        unsafe { sys::SetRXAAGCAttack(self.id, attack_ms); }
    }

    // --- FM demod params -----------------------------------------------

    pub fn set_fm_deviation(&mut self, hz: f64) {
        unsafe { sys::SetRXAFMDeviation(self.id, hz); }
    }
    pub fn set_ctcss_enabled(&mut self, enabled: bool) {
        unsafe { sys::SetRXACTCSSRun(self.id, i32::from(enabled)); }
    }
    pub fn set_ctcss_freq(&mut self, hz: f64) {
        unsafe { sys::SetRXACTCSSFreq(self.id, hz); }
    }

    // --- TNF (tracking notch filter) -----------------------------------

    pub fn set_tnf_enabled(&mut self, enabled: bool) {
        unsafe { sys::RXANBPSetNotchesRun(self.id, i32::from(enabled)); }
    }

    /// Append a notch at index `idx`. Indices must be contiguous from 0.
    pub fn add_tnf_notch(&mut self, idx: u32, fcenter: f64, fwidth: f64, active: bool) -> bool {
        // SAFETY: channel id valid; backing store is WDSP's own notch DB.
        let rc = unsafe {
            sys::RXANBPAddNotch(self.id, idx as i32, fcenter, fwidth, i32::from(active))
        };
        rc == 0
    }
    pub fn edit_tnf_notch(&mut self, idx: u32, fcenter: f64, fwidth: f64, active: bool) -> bool {
        let rc = unsafe {
            sys::RXANBPEditNotch(self.id, idx as i32, fcenter, fwidth, i32::from(active))
        };
        rc == 0
    }
    pub fn delete_tnf_notch(&mut self, idx: u32) -> bool {
        let rc = unsafe { sys::RXANBPDeleteNotch(self.id, idx as i32) };
        rc == 0
    }
    pub fn num_tnf_notches(&self) -> u32 {
        let mut n: i32 = 0;
        unsafe { sys::RXANBPGetNumNotches(self.id, &mut n) };
        n.max(0) as u32
    }

    // --- SAM sub-mode (AMD) --------------------------------------------

    /// 0 = DSB, 1 = LSB, 2 = USB.
    pub fn set_sam_submode(&mut self, submode: u8) {
        unsafe { sys::SetRXAAMDSBMode(self.id, submode as i32); }
    }

    // --- BPSNBA tuning -------------------------------------------------

    /// BPSNBA auto-routes when SNBA + TNF are both active. These two
    /// knobs only adjust the internal filter; there's no separate
    /// run toggle by design.
    pub fn set_bpsnba_nc(&mut self, nc: u32) {
        unsafe { sys::RXABPSNBASetNC(self.id, nc as i32); }
    }
    pub fn set_bpsnba_mp(&mut self, mp: bool) {
        unsafe { sys::RXABPSNBASetMP(self.id, i32::from(mp)); }
    }

    // --- NB / NB2 (external time-domain noise blankers) ---------------

    pub fn set_nb_enabled(&mut self, enabled: bool) {
        unsafe { sys::SetEXTANBRun(self.id, i32::from(enabled)); }
    }
    pub fn set_nb_threshold(&mut self, threshold: f64) {
        unsafe { sys::SetEXTANBThreshold(self.id, threshold); }
    }
    pub fn set_nb2_enabled(&mut self, enabled: bool) {
        unsafe { sys::SetEXTNOBRun(self.id, i32::from(enabled)); }
    }
    pub fn set_nb2_threshold(&mut self, threshold: f64) {
        unsafe { sys::SetEXTNOBThreshold(self.id, threshold); }
    }
    pub fn set_nb2_mode(&mut self, mode: i32) {
        unsafe { sys::SetEXTNOBMode(self.id, mode); }
    }

    /// Enable or disable binaural (true stereo) audio output.
    pub fn set_binaural(&mut self, enabled: bool) {
        unsafe { sys::SetRXAPanelBinaural(self.id, i32::from(enabled)); }
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
        // SAFETY: buffer sizes verified above, channel id is valid.
        // xanbEXT / xnobEXT run in place; they no-op when their
        // internal `run` flag is 0, so we always call them regardless
        // of the user-visible NB toggles. The Thetis RXA pipeline
        // does exactly this — NB lives outside the RXA critical path.
        unsafe {
            sys::xanbEXT(self.id, in_buf.as_mut_ptr(), in_buf.as_mut_ptr());
            sys::xnobEXT(self.id, in_buf.as_mut_ptr(), in_buf.as_mut_ptr());
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
        unsafe {
            sys::destroy_anbEXT(self.id);
            sys::destroy_nobEXT(self.id);
            sys::CloseChannel(self.id);
        }
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
