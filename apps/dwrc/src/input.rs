//! Keyboard and gamepad input mapped onto a PlayStation digital pad.

use dwrc_psx::{Button, PadState};
use dwrc_runtime::config::InputConfig;
use eframe::egui;
use gilrs::{Axis, Button as G, EventType};

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

/// A gamepad button, or one direction of a stick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PadInput {
    Button(G),
    /// An axis pushed past half way, towards its positive end or not.
    Axis(Axis, bool),
}

/// Gamepad inputs that can be bound: (name in `settings.toml`, input, label).
/// Face buttons are named by position, as gamepads label them differently.
pub const PAD_INPUTS: [(&str, PadInput, &str); 27] = [
    ("South", PadInput::Button(G::South), "A / Cross"),
    ("East", PadInput::Button(G::East), "B / Circle"),
    ("West", PadInput::Button(G::West), "X / Square"),
    ("North", PadInput::Button(G::North), "Y / Triangle"),
    ("DPadUp", PadInput::Button(G::DPadUp), "D-pad Up"),
    ("DPadDown", PadInput::Button(G::DPadDown), "D-pad Down"),
    ("DPadLeft", PadInput::Button(G::DPadLeft), "D-pad Left"),
    ("DPadRight", PadInput::Button(G::DPadRight), "D-pad Right"),
    ("Start", PadInput::Button(G::Start), "Start / Menu"),
    ("Select", PadInput::Button(G::Select), "Back / Select"),
    ("Mode", PadInput::Button(G::Mode), "Guide / Home"),
    ("LeftShoulder", PadInput::Button(G::LeftTrigger), "LB / L1"),
    ("LeftTrigger", PadInput::Button(G::LeftTrigger2), "LT / L2"),
    ("RightShoulder", PadInput::Button(G::RightTrigger), "RB / R1"),
    ("RightTrigger", PadInput::Button(G::RightTrigger2), "RT / R2"),
    ("LeftStickPress", PadInput::Button(G::LeftThumb), "Left stick press"),
    ("RightStickPress", PadInput::Button(G::RightThumb), "Right stick press"),
    ("LeftStickUp", PadInput::Axis(Axis::LeftStickY, true), "Left stick Up"),
    ("LeftStickDown", PadInput::Axis(Axis::LeftStickY, false), "Left stick Down"),
    ("LeftStickLeft", PadInput::Axis(Axis::LeftStickX, false), "Left stick Left"),
    ("LeftStickRight", PadInput::Axis(Axis::LeftStickX, true), "Left stick Right"),
    ("RightStickUp", PadInput::Axis(Axis::RightStickY, true), "Right stick Up"),
    ("RightStickDown", PadInput::Axis(Axis::RightStickY, false), "Right stick Down"),
    ("RightStickLeft", PadInput::Axis(Axis::RightStickX, false), "Right stick Left"),
    ("RightStickRight", PadInput::Axis(Axis::RightStickX, true), "Right stick Right"),
    ("C", PadInput::Button(G::C), "Extra button C"),
    ("Z", PadInput::Button(G::Z), "Extra button Z"),
];

fn pad_input(name: &str) -> Option<PadInput> {
    PAD_INPUTS.iter().find(|(n, ..)| *n == name).map(|(_, i, _)| *i)
}

/// What the controls editor shows for a gamepad input name.
pub fn pad_label(name: &str) -> &str {
    PAD_INPUTS.iter().find(|(n, ..)| *n == name).map_or(name, |(.., l)| *l)
}

/// How far a stick has to move to count as pressed, and (further, so a
/// resting stick that drifts isn't picked up) to be bound in the editor.
const STICK_PRESS: f32 = 0.5;
const STICK_CAPTURE: f32 = 0.7;

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

    /// Current pad state from the keyboard and the first gamepad.
    pub fn poll(&mut self, ctx: &egui::Context, cfg: &InputConfig, keyboard_enabled: bool) -> PadState {
        let mut s = PadState::default();
        if keyboard_enabled {
            ctx.input(|i| {
                for (btn, key) in &cfg.keyboard {
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
                let down = |i: PadInput| match i {
                    PadInput::Button(b) => pad.is_pressed(b),
                    PadInput::Axis(a, true) => pad.value(a) > STICK_PRESS,
                    PadInput::Axis(a, false) => pad.value(a) < -STICK_PRESS,
                };
                for (btn, input) in &cfg.gamepad {
                    if let (Some(b), Some(i)) = (button_from_name(btn), pad_input(input)) {
                        if down(i) {
                            s.set(b, true);
                        }
                    }
                }
                // The left stick also acts as a D-pad (the games are digital).
                if cfg.stick_dpad {
                    let x = pad.value(Axis::LeftStickX);
                    let y = pad.value(Axis::LeftStickY);
                    s.set(Button::Left, s.pressed(Button::Left) || x < -STICK_PRESS);
                    s.set(Button::Right, s.pressed(Button::Right) || x > STICK_PRESS);
                    s.set(Button::Up, s.pressed(Button::Up) || y > STICK_PRESS);
                    s.set(Button::Down, s.pressed(Button::Down) || y < -STICK_PRESS);
                }
            }
        }
        s
    }

    /// Forgets queued gamepad events (before listening for a new binding).
    pub fn flush(&mut self) {
        if let Some(g) = &mut self.gilrs {
            while g.next_event().is_some() {}
        }
    }

    /// The name of the first gamepad input pressed since the last call, on
    /// any gamepad, for the controls editor.
    pub fn capture(&mut self) -> Option<&'static str> {
        let g = self.gilrs.as_mut()?;
        let mut found = None;
        while let Some(e) = g.next_event() {
            let input = match e.event {
                EventType::ButtonPressed(b, _) => PadInput::Button(b),
                EventType::AxisChanged(a, v, _) if v.abs() > STICK_CAPTURE => PadInput::Axis(a, v > 0.0),
                _ => continue,
            };
            if found.is_none() {
                found = PAD_INPUTS.iter().find(|(_, i, _)| *i == input).map(|(n, ..)| *n);
            }
        }
        found
    }
}

/// Binds `input` to `button`. A PlayStation button that had this input
/// already takes `button`'s old one instead, so one press never means two
/// buttons and nothing becomes unbound.
pub fn bind_gamepad(map: &mut std::collections::BTreeMap<String, String>, button: Button, input: &str) {
    let name = button_name(button);
    let old = map.insert(name.clone(), input.to_string());
    let other = map.iter().find(|(b, i)| **b != name && *i == input).map(|(b, _)| b.clone());
    if let Some(other) = other {
        match old {
            Some(o) => map.insert(other, o),
            None => map.remove(&other),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bindings_are_known() {
        let d = InputConfig::default_gamepad();
        for b in EDITABLE {
            let input = &d[&button_name(b)];
            assert!(pad_input(input).is_some(), "{input}");
        }
        let names: std::collections::BTreeSet<_> = PAD_INPUTS.iter().map(|(n, ..)| *n).collect();
        assert_eq!(names.len(), PAD_INPUTS.len());
    }

    #[test]
    fn binding_swaps() {
        let mut m = InputConfig::default_gamepad();
        bind_gamepad(&mut m, Button::Cross, "East");
        assert_eq!((m["Cross"].as_str(), m["Circle"].as_str()), ("East", "South"));
        // An input nobody had: nothing else changes.
        bind_gamepad(&mut m, Button::Select, "RightStickPress");
        assert_eq!(m["Select"], "RightStickPress");
        assert_eq!(m.values().filter(|v| *v == "Select").count(), 0);
        // Taking an input from a button while unbound leaves that one unbound.
        m.remove("Start");
        bind_gamepad(&mut m, Button::Start, "South");
        assert_eq!(m["Start"], "South");
        assert!(!m.contains_key("Circle"));
    }
}
