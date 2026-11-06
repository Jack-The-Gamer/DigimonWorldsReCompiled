//! Digimon World 3 / 2003: the mouse rules. Addresses are from the
//! dw3_decomp decompilation (`config/<version>/symbols*.txt`; the three
//! executables match its builds byte for byte), names as there. Everything
//! in the game is a task (`TASK_HEADER`: state at 0xC, children at 0x24,
//! update at 0x48); the hooks note which menu cursors and text windows ran
//! each frame, and their fields say where they are.
//!
//! * Field: left presses confirm, right Start (the field menu).
//! * The title (STDWTITL): left presses Start; on New Game and Continue, it
//!   picks the one clicked too.
//! * The European disc's language screen (CNTY_SEL): the flag clicked is
//!   picked and taken (Start).
//! * Menus with the engine's cursor (`createCursor`: the field menu and its
//!   screens, shops, the Digimon Lab, choices): left steers the cursor onto
//!   the option clicked (the cursor and the text window it points at) and
//!   confirms, right cancels.
//! * Battle menus (`FIGHTSTG_createCursor`: commands, techniques, items,
//!   switching): the line clicked is picked and confirmed.
//! * The memory card screen's three saves (STGMCARD): the one clicked is
//!   picked (left and right) and confirmed.
//! * The field menu's screens (STSTATUS) choosing a party member: the panel
//!   clicked is chosen (up to the first, down to it) and confirmed.
//! * Elsewhere (dialogue, messages): left confirms, right cancels.

use super::{pad, steer, Click, Game, Input, Pad, Rect, Task};
use dwrc_disc::Identification;
use dwrc_psx::{Bus, HookAction, HookId, Psx};
use std::sync::{Arc, Mutex};

struct Addrs {
    serial: &'static str,
    sha1: &'static str,
    /// `PAD` (PadState): slot [0][0]'s held buttons at 0x4E.
    pad: u32,
    /// `GAME.mode`.
    game_mode: u32,
    update_cursor: u32,
    text_window_draw: u32,
    update_text_window: u32,
    /// In the FIGHTSTG overlay (only valid while it is loaded).
    fightstg_update_cursor: u32,
    /// `STGMCARD_funcs` (the STGMCARD overlay): the save picked at 4.
    stgmcard_funcs: u32,
    /// `CNTY_SEL_tickMenu` (the CNTY_SEL overlay): the language screen's task.
    cnty_sel_tick_menu: u32,
    /// Japan keeps the face buttons as they are (confirm is Circle, cancel
    /// Cross); the others swap them round (`readPadButtons`).
    japan: bool,
}

const VERSIONS: [Addrs; 3] = [
    Addrs {
        serial: "SLES-03936",
        sha1: "d1b7e4d646e3a9c2b88fdb25d20b5f7116bbb06d",
        pad: 0x8004B7D0,
        game_mode: 0x80048D34 + 0x26C4,
        update_cursor: 0x8001B0A4,
        text_window_draw: 0x800197A4,
        update_text_window: 0x8001AC14,
        fightstg_update_cursor: 0x8009AF4C,
        stgmcard_funcs: 0x80088768,
        cnty_sel_tick_menu: 0x80083D18,
        japan: false,
    },
    Addrs {
        serial: "SLUS-01436",
        sha1: "444653259f78ddb483fd22af72cce9276f42f214",
        pad: 0x8004AF78,
        game_mode: 0x800484E8 + 0x26BC,
        update_cursor: 0x8001AD20,
        text_window_draw: 0x80019420,
        update_text_window: 0x8001A890,
        fightstg_update_cursor: 0x8009A098,
        stgmcard_funcs: 0x80087D9C,
        cnty_sel_tick_menu: 0x800834B0,
        japan: false,
    },
    Addrs {
        serial: "SLPS-03446",
        sha1: "0e5b6b9ee2f99a804ed8c4117c805482c10e260b",
        pad: 0x8004B58C,
        game_mode: 0x80048AF0 + 0x26C4,
        update_cursor: 0x8001AF40,
        text_window_draw: 0x80019640,
        update_text_window: 0x8001AAB0,
        fightstg_update_cursor: 0x8009AAB4,
        stgmcard_funcs: 0x8008839C,
        cnty_sel_tick_menu: 0x80083AC8,
        japan: true,
    },
];

/// `GAME.mode` with the overlay's byte only (MODE_OVERLAY).
const MODE_CONTINUE: u32 = 0xC00;
const MODE_TITLE: u32 = 0xE00;
const MODE_COUNTRY_SELECT: u32 = 0x1600;
const MODE_STATUS: u32 = 0x1000;

/// Task header fields.
const TASK_STATE: u32 = 0x0C;
const TASK_SUBSTATE: u32 = 0x10;
const TASK_PAUSED: u32 = 0x1C;
const TASK_CHILDREN: u32 = 0x24;
const TASK_UPDATE: u32 = 0x48;
const TASK_RUN: u32 = 1;

/// The game's own pad bits (PAD_* in include/engine/pad.h) for each of
/// `pad::`'s, as `readPadButtons` makes them: Select..R1 as they come, then
/// Circle, Cross, Triangle, Square (rotated from the pad's Triangle, Circle,
/// Cross, except in Japan).
fn logical(japan: bool) -> [(u16, u16); 16] {
    let face = if japan {
        [(pad::TRIANGLE, 12), (pad::CIRCLE, 13), (pad::CROSS, 14)]
    } else {
        [(pad::TRIANGLE, 14), (pad::CIRCLE, 12), (pad::CROSS, 13)]
    };
    let b = |bit: u16| 1u16 << bit;
    let face = |p: u16| face.iter().find(|f| f.0 == p).map_or(0, |f| b(f.1));
    [
        (pad::SELECT, b(0)),
        (0x0200, b(1)),
        (0x0400, b(2)),
        (pad::START, b(3)),
        (pad::UP, b(4)),
        (pad::RIGHT, b(5)),
        (pad::DOWN, b(6)),
        (pad::LEFT, b(7)),
        (pad::L2, b(8)),
        (pad::R2, b(9)),
        (pad::L1, b(10)),
        (pad::R1, b(11)),
        (pad::TRIANGLE, face(pad::TRIANGLE)),
        (pad::CIRCLE, face(pad::CIRCLE)),
        (pad::CROSS, face(pad::CROSS)),
        (pad::SQUARE, b(15)),
    ]
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Cursor,
    Window,
    BattleCursor,
    CountryMenu,
}

/// The tasks the hooks saw run lately: (vsync, kind, task address).
type Seen = Arc<Mutex<Vec<(u64, Kind, u32)>>>;

/// A task counts as on screen for this many vsyncs after it ran (DW3 runs
/// at 30 frames a second, slower when busy).
const FRESH: u64 = 6;

pub(super) fn new(psx: &mut Psx, ident: &Identification) -> Option<(Box<dyn Game>, Option<Input>)> {
    let a = VERSIONS.iter().find(|a| a.serial == ident.serial && (ident.exe_sha1.is_empty() || ident.exe_sha1 == a.sha1));
    let Some(a) = a else {
        log::info!("mouse: no rules for {} (executable {})", ident.serial, ident.exe_sha1);
        return None;
    };
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = Vec::new();
    for (addr, kind) in [(a.update_cursor, Kind::Cursor), (a.text_window_draw, Kind::Window), (a.fightstg_update_cursor, Kind::BattleCursor), (a.cnty_sel_tick_menu, Kind::CountryMenu)] {
        let s = seen.clone();
        hooks.push(psx.bus.add_hook(
            addr,
            Box::new(move |cpu, bus| {
                let vsync = bus.gpu.frames;
                let mut s = s.lock().unwrap();
                s.retain(|e| e.0 <= vsync && e.0 + FRESH >= vsync);
                let t = cpu.regs[4];
                match s.iter_mut().find(|e| e.1 == kind && e.2 == t) {
                    Some(e) => e.0 = vsync,
                    None => s.push((vsync, kind, t)),
                }
                HookAction::Continue
            }),
        ));
    }
    let map = logical(a.japan);
    let held = a.pad + 0x48 + 6;
    let input: Input = Box::new(move |bus| {
        let l = bus.peek16(held);
        map.iter().filter(|(_, lb)| l & lb != 0).fold(0, |acc, (p, _)| acc | p)
    });
    let (confirm, cancel) = if a.japan { (pad::CIRCLE, pad::CROSS) } else { (pad::CROSS, pad::TRIANGLE) };
    Some((Box::new(Dw3 { a, seen, hooks, confirm, cancel }), Some(input)))
}

struct Dw3 {
    a: &'static Addrs,
    seen: Seen,
    hooks: Vec<HookId>,
    confirm: u16,
    cancel: u16,
}

fn s16(bus: &Bus, a: u32) -> i32 {
    bus.peek16(a) as i16 as i32
}

/// Whether `t` is a running task with this update function.
fn live(bus: &Bus, t: u32, update: u32) -> bool {
    t & 0x1F_FFFF != 0 && t & 3 == 0 && bus.peek32(t + TASK_UPDATE) == update && bus.peek32(t + TASK_STATE) == TASK_RUN && bus.peek32(t + TASK_PAUSED) == 0
}

/// Where a text window's text went when last drawn (`textWindowDraw`: the
/// pen starts at -alignWidth and ends at cursorX, cursorY lines down).
fn window_rect(bus: &Bus, w: u32) -> Option<Rect> {
    if bus.peek8(w + 0xC1) == 0 {
        return None;
    }
    let (x, y) = (s16(bus, w + 0xB0), s16(bus, w + 0xB2));
    let (end_x, end_y, align) = (s16(bus, w + 0xB8), bus.peek16(w + 0xBA) as i32, s16(bus, w + 0xBC));
    let line = s16(bus, w + 0xB6).clamp(12, 24);
    let width = if end_y > 0 { 320 - x.max(0) } else { end_x + align };
    (width > 0).then(|| Rect::new(x - align, y - 2, width, end_y + line))
}

impl Dw3 {
    fn tasks(&self, kind: Kind) -> Vec<u32> {
        self.seen.lock().unwrap().iter().filter(|e| e.1 == kind).map(|e| e.2).collect()
    }

    fn windows(&self, bus: &Bus) -> Vec<(u32, Rect)> {
        self.tasks(Kind::Window).into_iter().filter(|&w| live(bus, w, self.a.update_text_window)).filter_map(|w| window_rect(bus, w).map(|r| (w, r))).collect()
    }

    /// The engine cursor showing, if any: its task.
    fn cursor(&self, bus: &Bus) -> Option<u32> {
        self.tasks(Kind::Cursor).into_iter().rev().find(|&c| live(bus, c, self.a.update_cursor) && bus.peek32(c + 0x64) != 0)
    }

    /// The battle menu cursor taking presses, if any.
    fn battle_cursor(&self, bus: &Bus) -> Option<u32> {
        self.tasks(Kind::BattleCursor).into_iter().rev().find(|&c| live(bus, c, self.a.fightstg_update_cursor) && bus.peek32(c + 0x54) == 0)
    }
}

/// The engine cursor and the text window it points at, as one box: the
/// nearest window starting just right of the cursor on its line.
fn pointed(bus: &Bus, a: &'static Addrs, seen: &Seen, c: u32) -> Option<Rect> {
    if !live(bus, c, a.update_cursor) || bus.peek32(c + 0x64) == 0 {
        return None;
    }
    let (cx, cy) = (bus.peek32(c + 0x58) as i32, bus.peek32(c + 0x5C) as i32);
    let own = bus.peek32(bus.peek32(c + TASK_CHILDREN));
    let glyph = window_rect(bus, own).unwrap_or(Rect::new(cx, cy, 12, 14));
    let mid = glyph.y + glyph.h / 2;
    let windows: Vec<u32> = seen.lock().unwrap().iter().filter(|e| e.1 == Kind::Window && e.2 != own).map(|e| e.2).collect();
    let best = windows
        .into_iter()
        .filter(|&w| live(bus, w, a.update_text_window))
        .filter_map(|w| window_rect(bus, w))
        .filter(|r| r.x >= glyph.x + glyph.w / 2 && r.x <= glyph.x + glyph.w + 40 && r.y <= mid && mid < r.y + r.h.min(20))
        .min_by_key(|r| r.x);
    Some(match best {
        Some(r) => Rect::new(glyph.x, glyph.y.min(r.y), r.x + r.w - glyph.x, (glyph.y + glyph.h).max(r.y + r.h.min(20)) - glyph.y.min(r.y)),
        None => Rect::new(glyph.x, glyph.y, 100, glyph.h),
    })
}

impl Game for Dw3 {
    fn click(&mut self, psx: &mut Psx, c: Click, pad: &mut Pad) -> Option<Task> {
        let a = self.a;
        let bus = &mut psx.bus;
        // The game's drawing coordinates: the 320-wide picture's pixels.
        let (x, y) = (c.x * 320 / c.width.max(1), c.y * 240 / c.height.max(1));
        let mode = bus.peek32(a.game_mode) & 0xFF00;
        let field = mode == 0x200 || mode == 0x300;
        log::debug!("mouse: click ({x}, {y}){} in mode {mode:#x}", if c.right { " right" } else { "" });
        if let Some(m) = self.battle_cursor(bus) {
            if c.right {
                pad.tap(self.cancel);
                return None;
            }
            // FIGHTSTG_drawCursorBar: line i's bar at (x, y + i * step), the
            // line's text windows to its right, on a panel about 96 wide
            // (the commands' label boxes).
            let (count, bx, by, step) = (bus.peek32(m + 0x58) as i32, bus.peek32(m + 0x5C) as i32, bus.peek32(m + 0x60) as i32, bus.peek32(m + 0x64) as i32);
            let windows = self.windows(bus);
            for i in 0..count.clamp(0, 8) {
                let ly = by + i * step;
                let mut line = Rect::new(bx, ly, 96, step.max(12));
                for (_, r) in &windows {
                    let mid = r.y + r.h.min(20) / 2;
                    if r.x >= bx - 4 && mid >= ly && mid < ly + line.h {
                        line.w = line.w.max(r.x + r.w - bx);
                    }
                }
                if line.contains(x, y) {
                    log::debug!("mouse: battle menu line {i} of {count}");
                    if bus.peek32(m + 0x50) != i as u32 {
                        // The menus show the new line on the frame they see
                        // it, and take no confirm then (FIGHTSTG_updateTechMenu).
                        bus.poke32(m + 0x50, i as u32);
                        pad.wait(6);
                    }
                    pad.tap(self.confirm);
                    return None;
                }
            }
            return None;
        }
        if let Some(cur) = self.cursor(bus) {
            log::debug!("mouse: click ({x}, {y}) on a menu, cursor at {:?}", pointed(bus, a, &self.seen, cur));
            if c.right {
                pad.tap(self.cancel);
                return None;
            }
            let seen = self.seen.clone();
            return Some(steer(x, y, self.confirm, move |psx| pointed(&psx.bus, a, &seen, cur)));
        }
        if !c.right && mode == MODE_COUNTRY_SELECT {
            // CNTY_SEL_tickMenu: the languages a column of flags 34 apart from
            // y 48, the one picked at 0x50 (Up and Down move it, Start takes
            // it) once the panels are open (substate MENU_SELECT, 7).
            let row = (0..5).find(|k| Rect::new(160, 48 + k * 34, 150, 34).contains(x, y));
            let menu = self.tasks(Kind::CountryMenu).into_iter().rev().find(|&t| live(bus, t, a.cnty_sel_tick_menu));
            if let (Some(k), Some(t)) = (row, menu) {
                let mut waited = 0;
                return Some(Box::new(move |psx: &mut Psx, pad: &mut Pad| {
                    if !live(&psx.bus, t, a.cnty_sel_tick_menu) {
                        return false;
                    }
                    if psx.bus.peek32(t + TASK_SUBSTATE) != 7 {
                        waited += 1;
                        pad.wait(1);
                        return waited < 120;
                    }
                    // Shown highlighted for a moment, then taken.
                    psx.bus.poke16(t + 0x50, k as u16);
                    pad.wait(4);
                    pad.tap(pad::START);
                    false
                }));
            }
            pad.tap(pad::START);
            return None;
        }
        if !c.right && mode == MODE_TITLE {
            // STDWTITL_tickMenu waits for Start on "PRESS START", then moves
            // between New Game (y 154) and Continue (y 174, STDWTITL_menu-
            // CursorPositions) with up and down and takes cross; each press
            // does nothing in the other step.
            pad.tap(pad::START);
            if (60..260).contains(&x) && (146..192).contains(&y) {
                pad.tap(if y < 168 { pad::UP } else { pad::DOWN });
                pad.wait(4);
                pad.tap(self.confirm);
            }
            return None;
        }
        if !c.right && mode == MODE_CONTINUE {
            // STGMCARD_updateMenu: the saves 0x44 apart, left and right pick.
            if let Some(i) = (0..3).find(|i| Rect::new(98 + i * 0x44, 24, 60, 62).contains(x, y)) {
                let slot = a.stgmcard_funcs + 4;
                let confirm = self.confirm;
                let mut tries = 0;
                return Some(Box::new(move |psx: &mut Psx, pad: &mut Pad| {
                    let now = psx.bus.peek32(slot) as i32;
                    tries += 1;
                    if now == i || tries > 6 {
                        pad.wait(6);
                        pad.tap(confirm);
                        return false;
                    }
                    pad.tap(if i > now { pad::RIGHT } else { pad::LEFT });
                    true
                }));
            }
        }
        if !c.right && mode == MODE_STATUS {
            // The party's panels (STSTATUS_chooseStatusMember and the item
            // and technique screens'): 0x2E apart from y 0x11; up and down
            // stop at the ends.
            if let Some(k) = (0..3).find(|k| Rect::new(0, 0x11 + k * 0x2E, 150, 0x2C).contains(x, y)) {
                pad.tap(pad::UP);
                pad.tap(pad::UP);
                for _ in 0..k {
                    pad.tap(pad::DOWN);
                }
                pad.wait(4);
                pad.tap(self.confirm);
                return None;
            }
        }
        pad.tap(match (c.right, field) {
            (false, _) => self.confirm,
            (true, true) => pad::START,
            (true, false) => self.cancel,
        });
        None
    }

    fn reset(&mut self) {
        self.seen.lock().unwrap().clear();
    }

    fn remove(&mut self, psx: &mut Psx) {
        for h in self.hooks.drain(..) {
            psx.bus.remove_hook(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_buttons() {
        let bit = |japan: bool, p: u16| logical(japan).iter().find(|m| m.0 == p).unwrap().1;
        // The menus confirm with PAD_CROSS (13) and cancel with PAD_TRIANGLE (14).
        assert_eq!((bit(false, pad::CROSS), bit(false, pad::TRIANGLE), bit(false, pad::CIRCLE)), (1 << 13, 1 << 14, 1 << 12));
        // Japan: Circle confirms, Cross cancels.
        assert_eq!((bit(true, pad::CIRCLE), bit(true, pad::CROSS), bit(true, pad::TRIANGLE)), (1 << 13, 1 << 14, 1 << 12));
        assert_eq!(bit(true, pad::START), 1 << 3);
        let all: u16 = logical(false).iter().fold(0, |a, m| a | m.1);
        assert_eq!(all, 0xFFFF);
    }
}
