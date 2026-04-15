//! Digital-mode pipeline types.
//!
//! Thin architectural slice: public enums and decode payload consumed
//! by `arion-app`, the Rhai script API, and the egui view. Actual
//! demod/decode logic lives behind a decoder trait and is filled in
//! incrementally (PSK31 first, then RTTY, APRS, channelizer).

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum DigitalMode {
    Psk31,
    Psk63,
    Rtty,
    Aprs,
}

impl DigitalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Psk31 => "psk31",
            Self::Psk63 => "psk63",
            Self::Rtty => "rtty",
            Self::Aprs => "aprs",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "psk31" => Some(Self::Psk31),
            "psk63" => Some(Self::Psk63),
            "rtty" => Some(Self::Rtty),
            "aprs" => Some(Self::Aprs),
            _ => None,
        }
    }
}

/// A decoded unit of information from a digital mode pipeline.
#[derive(Debug, Clone)]
pub struct DigitalDecode {
    pub mode: DigitalMode,
    pub text: String,
    pub snr_db: f32,
}
