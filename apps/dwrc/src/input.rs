//! Keyboard and gamepad input mapped onto a PlayStation digital pad.

use dwrc_psx::{Button, PadState};
use eframe::egui;
use std::collections::BTreeMap;

pub fn button_name(b: Button) -> String {
    format!("{b:?}")
}

pub fn button_from_name(s: &str) -> Option<Button> {
    Button::ALL.into_iter().find(|b| button_name(*b) == s)
}

/// Buttons shown in the controls editor, in a friendly order.
pub const EDITABLE: [Button; 14] = [
    Button::Up,
    Button::Down,
    Button::Left,
    Button::Right,
    Button::Cross,
    Button::Circle,
    Button::Square,
    Button::Triangle,
    Button::Start,
    Button::Select,
    Button::L1,
    Button::R1,
    Button::L2,
    Button::R2,
];

pub struct Input {
    gilrs: Option<gilrs::Gilrs>,
}

impl Input {
    pub fn new() -> Self {
        let gilrs = gilrs::Gilrs::new().map_err(|e| log::warn!("gamepads unavailable: {e}")).ok();
        Self { gilrs }
    }

    pub fn gamepad_names(&self) -> Vec<String> {
        self.gilrs.as_ref().map(|g| g.gamepads().map(|(_, p)| p.name().to_string()).collect()).unwrap_or_default()
    }

    /// Current pad state from keyboard bindings and the first gamepad.
    pub fn poll(&mut self, ctx: &egui::Context, bindings: &BTreeMap<String, String>, keyboard_enabled: bool) -> PadState {
        let mut s = PadState::default();
        if keyboard_enabled {
            ctx.input(|i| {
                for (btn, key) in bindings {
                    if let (Some(b), Some(k)) = (button_from_name(btn), egui::Key::from_name(key)) {
                        if i.key_down(k) {
                            s.set(b, true);
                        }
                    }
                }
            });
        }
        if let Some(g) = &mut self.gilrs {
            while g.next_event().is_some() {}
            if let Some((_, pad)) = g.gamepads().next() {
                use gilrs::Button as G;
                let map = [
                    (G::South, Button::Cross),
                    (G::East, Button::Circle),
                    (G::West, Button::Square),
                    (G::North, Button::Triangle),
                    (G::Start, Button::Start),
                    (G::Select, Button::Select),
                    (G::LeftTrigger, Button::L1),
                    (G::RightTrigger, Button::R1),
                    (G::LeftTrigger2, Button::L2),
                    (G::RightTrigger2, Button::R2),
                    (G::LeftThumb, Button::L3),
                    (G::RightThumb, Button::R3),
                    (G::DPadUp, Button::Up),
                    (G::DPadDown, Button::Down),
                    (G::DPadLeft, Button::Left),
                    (G::DPadRight, Button::Right),
                ];
                for (gb, b) in map {
                    if pad.is_pressed(gb) {
                        s.set(b, true);
                    }
                }
                // Left stick acts as a D-pad (the original games are digital).
                let x = pad.value(gilrs::Axis::LeftStickX);
                let y = pad.value(gilrs::Axis::LeftStickY);
                if x < -0.5 {
                    s.set(Button::Left, true);
                }
                if x > 0.5 {
                    s.set(Button::Right, true);
                }
                if y > 0.5 {
                    s.set(Button::Up, true);
                }
                if y < -0.5 {
                    s.set(Button::Down, true);
                }
            }
        }
        s
    }
}
