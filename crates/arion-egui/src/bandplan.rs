//! Ham radio bandplan overlays drawn behind the spectrum curve.
//!
//! Segments cover the 160m–6m IARU HF/VHF allocations. Each segment
//! carries a `SegmentKind` used to pick a subtle background tint so the
//! operator can see at a glance which mode sub-band the VFO is in.

use arion_settings::BandplanRegion;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke};

/// Activity type for a bandplan segment. Drives the overlay colour.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    Cw,
    Ssb,
    Digital,
    Fm,
    Mixed,
    Beacon,
}

impl SegmentKind {
    /// Semi-transparent fill (alpha ~40/255) drawn behind the spectrum.
    fn color(self) -> Color32 {
        match self {
            SegmentKind::Cw      => Color32::from_rgba_unmultiplied(  0, 200,   0, 40),
            SegmentKind::Ssb     => Color32::from_rgba_unmultiplied(  0,  80, 200, 40),
            SegmentKind::Digital => Color32::from_rgba_unmultiplied(140,   0, 200, 40),
            SegmentKind::Fm      => Color32::from_rgba_unmultiplied(  0, 180, 180, 40),
            SegmentKind::Mixed   => Color32::from_rgba_unmultiplied(100, 100, 100, 30),
            SegmentKind::Beacon  => Color32::from_rgba_unmultiplied(200, 150,   0, 40),
        }
    }

    fn label(self) -> &'static str {
        match self {
            SegmentKind::Cw      => "CW",
            SegmentKind::Ssb     => "SSB",
            SegmentKind::Digital => "DIG",
            SegmentKind::Fm      => "FM",
            SegmentKind::Mixed   => "MIX",
            SegmentKind::Beacon  => "BCN",
        }
    }
}

/// A single frequency range with an associated activity kind.
#[derive(Clone, Copy)]
pub struct Segment {
    pub lo:   u32,
    pub hi:   u32,
    pub kind: SegmentKind,
}

/// Return the segment table for a given IARU region. `Off` returns an
/// empty slice so callers can avoid an extra branch.
pub fn segments(region: BandplanRegion) -> &'static [Segment] {
    match region {
        BandplanRegion::Off     => &[],
        BandplanRegion::Region1 => REGION1,
        BandplanRegion::Region2 => REGION2,
        BandplanRegion::Region3 => REGION3,
    }
}

/// Paint the bandplan overlay into `rect` for the visible [lo_hz, hi_hz]
/// range. Segments fully outside the visible window are skipped; partly-
/// visible segments are clipped to the rect.
pub fn draw(painter: &egui::Painter, rect: Rect, lo_hz: f32, hi_hz: f32,
            region: BandplanRegion) {
    let span = hi_hz - lo_hz;
    if span <= 0.0 { return; }
    for s in segments(region) {
        let slo = s.lo as f32;
        let shi = s.hi as f32;
        if shi <= lo_hz || slo >= hi_hz { continue; }
        let x0 = rect.left() + ((slo - lo_hz) / span).clamp(0.0, 1.0) * rect.width();
        let x1 = rect.left() + ((shi - lo_hz) / span).clamp(0.0, 1.0) * rect.width();
        if x1 - x0 < 0.5 { continue; }
        let seg_rect = Rect::from_min_max(
            Pos2::new(x0, rect.top()),
            Pos2::new(x1, rect.bottom()),
        );
        painter.rect_filled(seg_rect, 0.0, s.kind.color());
        if x1 - x0 >= 20.0 {
            painter.text(
                Pos2::new((x0 + x1) * 0.5, rect.top() + 2.0),
                egui::Align2::CENTER_TOP,
                s.kind.label(),
                egui::FontId::proportional(9.0),
                Color32::from_rgba_unmultiplied(240, 240, 240, 220),
            );
        }
        painter.line_segment(
            [Pos2::new(x0, rect.top()), Pos2::new(x0, rect.bottom())],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(200, 200, 200, 140)),
        );
    }
}

// --- Segment tables ------------------------------------------------------
//
// Numbers come from the IARU bandplans (Region 1: 2020 update,
// Region 2: IARU R2 2016, Region 3: IARU R3 2019). Boundaries in Hz.
// Only the most-used HF bands + 6 m are covered — good enough for the
// 160–50 MHz range Arion currently reaches.

const KHZ: u32 = 1_000;
const MHZ: u32 = 1_000_000;

macro_rules! seg {
    ($lo:expr, $hi:expr, $kind:ident) => {
        Segment { lo: $lo, hi: $hi, kind: SegmentKind::$kind }
    };
}

static REGION1: &[Segment] = &[
    // 160 m
    seg!(1_810 * KHZ, 1_838 * KHZ, Cw),
    seg!(1_838 * KHZ, 1_840 * KHZ, Digital),
    seg!(1_840 * KHZ, 2_000 * KHZ, Ssb),
    // 80 m
    seg!(3_500 * KHZ, 3_570 * KHZ, Cw),
    seg!(3_570 * KHZ, 3_600 * KHZ, Digital),
    seg!(3_600 * KHZ, 3_800 * KHZ, Ssb),
    // 60 m (WRC-15 allocation)
    seg!(5_351 * KHZ, 5_366 * KHZ, Mixed),
    // 40 m
    seg!(7_000 * KHZ, 7_040 * KHZ, Cw),
    seg!(7_040 * KHZ, 7_050 * KHZ, Digital),
    seg!(7_050 * KHZ, 7_200 * KHZ, Ssb),
    // 30 m
    seg!(10_100 * KHZ, 10_140 * KHZ, Cw),
    seg!(10_140 * KHZ, 10_150 * KHZ, Digital),
    // 20 m
    seg!(14_000 * KHZ, 14_070 * KHZ, Cw),
    seg!(14_070 * KHZ, 14_099 * KHZ, Digital),
    seg!(14_099 * KHZ, 14_101 * KHZ, Beacon),
    seg!(14_101 * KHZ, 14_350 * KHZ, Ssb),
    // 17 m
    seg!(18_068 * KHZ, 18_095 * KHZ, Cw),
    seg!(18_095 * KHZ, 18_109 * KHZ, Digital),
    seg!(18_109 * KHZ, 18_111 * KHZ, Beacon),
    seg!(18_111 * KHZ, 18_168 * KHZ, Ssb),
    // 15 m
    seg!(21_000 * KHZ, 21_070 * KHZ, Cw),
    seg!(21_070 * KHZ, 21_149 * KHZ, Digital),
    seg!(21_149 * KHZ, 21_151 * KHZ, Beacon),
    seg!(21_151 * KHZ, 21_450 * KHZ, Ssb),
    // 12 m
    seg!(24_890 * KHZ, 24_915 * KHZ, Cw),
    seg!(24_915 * KHZ, 24_929 * KHZ, Digital),
    seg!(24_929 * KHZ, 24_931 * KHZ, Beacon),
    seg!(24_931 * KHZ, 24_990 * KHZ, Ssb),
    // 10 m
    seg!(28_000 * KHZ, 28_070 * KHZ, Cw),
    seg!(28_070 * KHZ, 28_190 * KHZ, Digital),
    seg!(28_190 * KHZ, 28_225 * KHZ, Beacon),
    seg!(28_225 * KHZ, 29_200 * KHZ, Ssb),
    seg!(29_200 * KHZ, 29_300 * KHZ, Fm),
    seg!(29_300 * KHZ, 29_700 * KHZ, Mixed),
    // 6 m
    seg!(50 * MHZ,       50_100 * KHZ, Cw),
    seg!(50_100 * KHZ,   50_300 * KHZ, Ssb),
    seg!(50_300 * KHZ,   50_500 * KHZ, Digital),
    seg!(50_500 * KHZ,   52 * MHZ,     Fm),
];

static REGION2: &[Segment] = &[
    // 160 m
    seg!(1_800 * KHZ, 1_840 * KHZ, Cw),
    seg!(1_840 * KHZ, 2_000 * KHZ, Ssb),
    // 80 m
    seg!(3_500 * KHZ, 3_600 * KHZ, Cw),
    seg!(3_600 * KHZ, 3_700 * KHZ, Digital),
    seg!(3_700 * KHZ, 4_000 * KHZ, Ssb),
    // 60 m
    seg!(5_330 * KHZ, 5_405 * KHZ, Mixed),
    // 40 m
    seg!(7_000 * KHZ, 7_070 * KHZ, Cw),
    seg!(7_070 * KHZ, 7_125 * KHZ, Digital),
    seg!(7_125 * KHZ, 7_300 * KHZ, Ssb),
    // 30 m
    seg!(10_100 * KHZ, 10_140 * KHZ, Cw),
    seg!(10_140 * KHZ, 10_150 * KHZ, Digital),
    // 20 m
    seg!(14_000 * KHZ, 14_070 * KHZ, Cw),
    seg!(14_070 * KHZ, 14_099 * KHZ, Digital),
    seg!(14_099 * KHZ, 14_101 * KHZ, Beacon),
    seg!(14_101 * KHZ, 14_350 * KHZ, Ssb),
    // 17 m
    seg!(18_068 * KHZ, 18_095 * KHZ, Cw),
    seg!(18_095 * KHZ, 18_110 * KHZ, Digital),
    seg!(18_110 * KHZ, 18_168 * KHZ, Ssb),
    // 15 m
    seg!(21_000 * KHZ, 21_070 * KHZ, Cw),
    seg!(21_070 * KHZ, 21_200 * KHZ, Digital),
    seg!(21_200 * KHZ, 21_450 * KHZ, Ssb),
    // 12 m
    seg!(24_890 * KHZ, 24_920 * KHZ, Cw),
    seg!(24_920 * KHZ, 24_930 * KHZ, Digital),
    seg!(24_930 * KHZ, 24_990 * KHZ, Ssb),
    // 10 m
    seg!(28_000 * KHZ, 28_070 * KHZ, Cw),
    seg!(28_070 * KHZ, 28_300 * KHZ, Digital),
    seg!(28_300 * KHZ, 29_300 * KHZ, Ssb),
    seg!(29_300 * KHZ, 29_700 * KHZ, Fm),
    // 6 m
    seg!(50 * MHZ,       50_100 * KHZ, Cw),
    seg!(50_100 * KHZ,   50_300 * KHZ, Ssb),
    seg!(50_300 * KHZ,   50_600 * KHZ, Digital),
    seg!(50_600 * KHZ,   54 * MHZ,     Fm),
];

static REGION3: &[Segment] = &[
    // 160 m
    seg!(1_800 * KHZ, 1_875 * KHZ, Cw),
    seg!(1_875 * KHZ, 2_000 * KHZ, Ssb),
    // 80 m
    seg!(3_500 * KHZ, 3_535 * KHZ, Cw),
    seg!(3_535 * KHZ, 3_600 * KHZ, Digital),
    seg!(3_600 * KHZ, 3_900 * KHZ, Ssb),
    // 40 m
    seg!(7_000 * KHZ, 7_040 * KHZ, Cw),
    seg!(7_040 * KHZ, 7_060 * KHZ, Digital),
    seg!(7_060 * KHZ, 7_200 * KHZ, Ssb),
    // 30 m
    seg!(10_100 * KHZ, 10_140 * KHZ, Cw),
    seg!(10_140 * KHZ, 10_150 * KHZ, Digital),
    // 20 m
    seg!(14_000 * KHZ, 14_070 * KHZ, Cw),
    seg!(14_070 * KHZ, 14_099 * KHZ, Digital),
    seg!(14_099 * KHZ, 14_101 * KHZ, Beacon),
    seg!(14_101 * KHZ, 14_350 * KHZ, Ssb),
    // 17 m
    seg!(18_068 * KHZ, 18_095 * KHZ, Cw),
    seg!(18_095 * KHZ, 18_110 * KHZ, Digital),
    seg!(18_110 * KHZ, 18_168 * KHZ, Ssb),
    // 15 m
    seg!(21_000 * KHZ, 21_070 * KHZ, Cw),
    seg!(21_070 * KHZ, 21_150 * KHZ, Digital),
    seg!(21_150 * KHZ, 21_450 * KHZ, Ssb),
    // 12 m
    seg!(24_890 * KHZ, 24_920 * KHZ, Cw),
    seg!(24_920 * KHZ, 24_930 * KHZ, Digital),
    seg!(24_930 * KHZ, 24_990 * KHZ, Ssb),
    // 10 m
    seg!(28_000 * KHZ, 28_070 * KHZ, Cw),
    seg!(28_070 * KHZ, 28_200 * KHZ, Digital),
    seg!(28_200 * KHZ, 29_300 * KHZ, Ssb),
    seg!(29_300 * KHZ, 29_700 * KHZ, Fm),
    // 6 m
    seg!(50 * MHZ,       50_100 * KHZ, Cw),
    seg!(50_100 * KHZ,   50_300 * KHZ, Ssb),
    seg!(50_300 * KHZ,   50_500 * KHZ, Digital),
    seg!(50_500 * KHZ,   54 * MHZ,     Fm),
];
