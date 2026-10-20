//! Digital controller (SCPH-1080) protocol.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Button {
    Select = 0,
    L3 = 1,
    R3 = 2,
    Start = 3,
    Up = 4,
    Right = 5,
    Down = 6,
    Left = 7,
    L2 = 8,
    R2 = 9,
    L1 = 10,
    R1 = 11,
    Triangle = 12,
    Circle = 13,
    Cross = 14,
    Square = 15,
}

impl Button {
    pub const ALL: [Button; 16] = [
        Button::Select,
        Button::L3,
        Button::R3,
        Button::Start,
        Button::Up,
        Button::Right,
        Button::Down,
        Button::Left,
        Button::L2,
        Button::R2,
        Button::L1,
        Button::R1,
        Button::Triangle,
        Button::Circle,
        Button::Cross,
        Button::Square,
    ];
}

/// Pressed-button state (bit set = pressed).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PadState {
    pub buttons: u16,
}

impl PadState {
    pub fn set(&mut self, b: Button, pressed: bool) {
        if pressed {
            self.buttons |= 1 << b as u16;
        } else {
            self.buttons &= !(1 << b as u16);
        }
    }
    pub fn pressed(&self, b: Button) -> bool {
        self.buttons & (1 << b as u16) != 0
    }
}

#[derive(Debug, Default, Clone)]
pub struct Pad {
    pub state: PadState,
    pub connected: bool,
    step: u8,
}

impl Pad {
    pub fn new() -> Self {
        Self { state: PadState::default(), connected: true, step: 0 }
    }

    pub fn disconnected() -> Self {
        Self { state: PadState::default(), connected: false, step: 0 }
    }

    pub fn reset_protocol(&mut self) {
        self.step = 0;
    }

    /// Exchanges one byte; returns (response, ack).
    pub fn transfer(&mut self, v: u8) -> (u8, bool) {
        if !self.connected {
            return (0xFF, false);
        }
        let b = !self.state.buttons;
        let r = match self.step {
            0 => {
                if v == 0x01 {
                    (0xFF, true)
                } else {
                    (0xFF, false)
                }
            }
            1 => {
                if v == 0x42 {
                    (0x41, true)
                } else {
                    self.step = 0;
                    return (0xFF, false);
                }
            }
            2 => (0x5A, true),
            3 => (b as u8, true),
            4 => ((b >> 8) as u8, false),
            _ => (0xFF, false),
        };
        self.step += 1;
        if !r.1 {
            self.step = 0;
        }
        r
    }
}

impl crate::state::Field for PadState {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        crate::sync_fields!(self, s; buttons);
    }
}

/// The buttons held are not restored: they are whatever the player holds now.
impl crate::state::Field for Pad {
    fn sync(&mut self, s: &mut crate::state::Sync) {
        crate::sync_fields!(self, s; connected, step);
    }
}
