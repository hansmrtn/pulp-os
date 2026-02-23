//! Button definitions and ADC decoding for XTEink X4
//!
//! The X4 uses resistance ladder circuits for most buttons.
//! Each ladder is read via ADC and decoded by comparing the
//! millivolt reading against known thresholds.

/// All physical buttons on the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    // Navigation cluster (Row 1 - GPIO1)
    Right,
    Left,
    Confirm,
    Back,
    // Volume buttons (Row 2 - GPIO2)
    VolUp,
    VolDown,
    // Discrete digital button
    Power,
}

impl Button {
    pub const fn name(self) -> &'static str {
        match self {
            Button::Right => "Right",
            Button::Left => "Left",
            Button::Confirm => "Confirm",
            Button::Back => "Back",
            Button::VolUp => "Vol Up",
            Button::VolDown => "Vol Down",
            Button::Power => "Power",
        }
    }
}

impl core::fmt::Display for Button {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

// ADC Threshold Tables
// Each entry: (center_mv, tolerance_mv, button)
// A reading matches if: center - tolerance <= reading <= center + tolerance
pub const DEFAULT_TOLERANCE: u16 = 150;

pub const ROW1_THRESHOLDS: &[(u16, u16, Button)] = &[
    (3, 50, Button::Right), // Near ground
    (1113, DEFAULT_TOLERANCE, Button::Left),
    (1984, DEFAULT_TOLERANCE, Button::Back),
    (2556, DEFAULT_TOLERANCE, Button::Confirm),
];

pub const ROW2_THRESHOLDS: &[(u16, u16, Button)] = &[
    (3, 50, Button::VolDown), // Near ground
    (1659, DEFAULT_TOLERANCE, Button::VolUp),
];

pub fn decode_ladder(mv: u16, thresholds: &[(u16, u16, Button)]) -> Option<Button> {
    for &(center, tolerance, button) in thresholds {
        let low = center.saturating_sub(tolerance);
        let high = center.saturating_add(tolerance);
        if mv >= low && mv <= high {
            return Some(button);
        }
    }
    None
}
