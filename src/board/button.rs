// Button definitions and ADC resistance ladder decoding
//
// Two ADC ladders (Row1 GPIO1, Row2 GPIO2) plus discrete power
// button on GPIO3. Each ladder encodes buttons as voltage levels.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Right,
    Left,
    Confirm,
    Back,
    VolUp,
    VolDown,
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

pub const DEFAULT_TOLERANCE: u16 = 150;

pub const ROW1_THRESHOLDS: &[(u16, u16, Button)] = &[
    (3, 50, Button::Right),
    (1113, DEFAULT_TOLERANCE, Button::Left),
    (1984, DEFAULT_TOLERANCE, Button::Confirm),
    (2556, DEFAULT_TOLERANCE, Button::Back),
];

pub const ROW2_THRESHOLDS: &[(u16, u16, Button)] = &[
    (3, 50, Button::VolDown),
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
