//! Mouse support: clicks on the picture become the presses a player would
//! make on the pad. Each game has its own rules (`dw1`, `dw3`): what a click
//! on the field, in a menu or in battle means, worked out from the game's
//! own state in RAM (which menu is open, where its cursor is, the battle
//! commands). A click on a menu option moves the game's cursor there with
//! the D-pad, one press at a time, and confirms.
//!
//! Nothing here is part of the console: the game only ever sees the pad,
//! the mouse's presses added to the keyboard's and the gamepad's. A press is
//! held until the game has read it (its own copy of the pad, when the rules
//! know where it is) and released until it has seen that too, so a game
//! that reads the pad only every few frames misses none.

use dwrc_disc::{GameId, Identification};
use dwrc_psx::{Bus, Psx};
use std::collections::VecDeque;

mod dw1;
mod dw3;

/// A click on the picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Click {
    pub right: bool,
    /// Where, in the console's display pixels from the top left of its
    /// picture (the 4:3 part; may be outside it with widescreen).
    pub x: i32,
    pub y: i32,
    /// The size of the console's picture in display pixels.
    pub width: i32,
    pub height: i32,
}

/// Pad buttons as the games read them (libetc's `PadRead` order).
pub mod pad {
    pub const L2: u16 = 0x0001;
    pub const R2: u16 = 0x0002;
    pub const L1: u16 = 0x0004;
    pub const R1: u16 = 0x0008;
    pub const TRIANGLE: u16 = 0x0010;
    pub const CIRCLE: u16 = 0x0020;
    pub const CROSS: u16 = 0x0040;
    pub const SQUARE: u16 = 0x0080;
    pub const SELECT: u16 = 0x0100;
    pub const START: u16 = 0x0800;
    pub const UP: u16 = 0x1000;
    pub const RIGHT: u16 = 0x2000;
    pub const DOWN: u16 = 0x4000;
    pub const LEFT: u16 = 0x8000;
}

/// Vsyncs a press is held (and released) at most while waiting for the game
/// to see it, and at least when the rules don't know where the game keeps
/// the pad.
const SEEN_TIMEOUT: u32 = 12;
const BLIND_FRAMES: u32 = 3;
/// Clicks waiting at most (mashing a button through a finisher).
const QUEUE: usize = 8;
/// Vsyncs the rules watch the game before acting on a click (after mouse
/// support starts, a reset or a state load), so that their hooks have seen
/// what is on screen.
const WARM_UP: u32 = 8;

enum Step {
    /// Press and release once.
    Tap(u16),
    /// Hold for this many vsyncs, then release.
    Hold(u16, u32),
    /// Nothing for this many vsyncs.
    Wait(u32),
}

/// The buttons the game read last (`pad` order), where the rules know it.
pub(crate) type Input = Box<dyn Fn(&Bus) -> u16 + Send>;

/// The presses being made.
pub(crate) struct Pad {
    /// The pad as the game last read it, if known.
    input: Option<Input>,
    steps: VecDeque<Step>,
    /// The step under way: held (true) or released, and for how many vsyncs.
    cur: Option<(Step, bool, u32)>,
}

impl Pad {
    fn new(input: Option<Input>) -> Self {
        Self { input, steps: VecDeque::new(), cur: None }
    }

    pub fn tap(&mut self, b: u16) {
        self.steps.push_back(Step::Tap(b));
    }

    pub fn hold(&mut self, b: u16, vsyncs: u32) {
        self.steps.push_back(Step::Hold(b, vsyncs));
    }

    pub fn wait(&mut self, vsyncs: u32) {
        self.steps.push_back(Step::Wait(vsyncs));
    }

    pub fn busy(&self) -> bool {
        self.cur.is_some() || !self.steps.is_empty()
    }

    fn clear(&mut self) {
        self.steps.clear();
        self.cur = None;
    }

    /// The buttons held now (game bit order).
    fn held(&mut self) -> u16 {
        if self.cur.is_none() {
            self.cur = self.steps.pop_front().map(|s| (s, true, 0));
        }
        match &self.cur {
            Some((Step::Tap(b) | Step::Hold(b, _), true, _)) => *b,
            _ => 0,
        }
    }

    /// After a frame: moves the step under way on.
    fn tick(&mut self, bus: &Bus) {
        let seen = self.input.as_ref().map(|f| f(bus));
        let Some((step, down, n)) = &mut self.cur else { return };
        *n += 1;
        let done = match step {
            Step::Wait(k) => *n >= *k,
            Step::Hold(b, k) if *down => *n >= *k && seen.is_none_or(|s| s & *b == *b || *n >= *k + SEEN_TIMEOUT),
            Step::Tap(b) if *down => match seen {
                Some(s) => s & *b == *b || *n >= SEEN_TIMEOUT,
                None => *n >= BLIND_FRAMES,
            },
            Step::Tap(b) | Step::Hold(b, _) => match seen {
                Some(s) => s & *b == 0 || *n >= SEEN_TIMEOUT,
                None => *n >= BLIND_FRAMES,
            },
        };
        if done {
            match step {
                Step::Tap(_) | Step::Hold(..) if *down => {
                    *down = false;
                    *n = 0;
                }
                _ => self.cur = None,
            }
        }
    }
}

/// Work that takes more than one go (moving a cursor step by step): called
/// once a frame whenever the pad is idle, until it returns false.
pub(crate) type Task = Box<dyn FnMut(&mut Psx, &mut Pad) -> bool + Send>;

/// One game's rules.
pub(crate) trait Game: Send {
    /// What a click does now: presses on `pad`, maybe a task to follow.
    fn click(&mut self, psx: &mut Psx, c: Click, pad: &mut Pad) -> Option<Task>;
    /// Forgets what the rules saw (a reset, a state loaded).
    fn reset(&mut self) {}
    /// Takes away anything the rules added to the console (hooks).
    fn remove(&mut self, psx: &mut Psx);
}

/// Mouse support for one session.
pub struct Mouse {
    game: Box<dyn Game>,
    pad: Pad,
    task: Option<Task>,
    clicks: VecDeque<Click>,
    /// Vsyncs watched since the start, a reset or a state load.
    watched: u32,
}

impl Mouse {
    /// Mouse support for the game on the disc, if it has rules for it.
    pub fn new(psx: &mut Psx, ident: &Identification) -> Option<Self> {
        let (game, input): (Box<dyn Game>, Option<Input>) = match ident.game {
            GameId::Dw1 => dw1::new(psx, ident)?,
            GameId::Dw3 => dw3::new(psx, ident)?,
            GameId::Dw2 => return None,
        };
        Some(Self { game, pad: Pad::new(input), task: None, clicks: VecDeque::new(), watched: 0 })
    }

    /// Whether this game has mouse rules (for the launcher to say so).
    pub fn supported(game: GameId) -> bool {
        game != GameId::Dw2
    }

    pub fn click(&mut self, c: Click) {
        if self.clicks.len() < QUEUE {
            self.clicks.push_back(c);
        }
    }

    /// The buttons the mouse holds for the next frame (`PadState` order).
    pub fn buttons(&mut self) -> u16 {
        self.pad.held().swap_bytes()
    }

    /// After a frame: moves the presses on, and starts on the next click
    /// once the last one is done.
    pub fn after_frame(&mut self, psx: &mut Psx) {
        self.pad.tick(&psx.bus);
        self.watched = self.watched.saturating_add(1);
        if self.pad.busy() || self.watched < WARM_UP {
            return;
        }
        if let Some(t) = &mut self.task {
            if !t(psx, &mut self.pad) {
                self.task = None;
            }
            return;
        }
        if let Some(mut c) = self.clicks.pop_front() {
            // With Widescreen UI, menus near an edge are shown moved out to it.
            c.x = psx.bus.gpu.ui_origin(c.x, c.y);
            self.task = self.game.click(psx, c, &mut self.pad);
        }
    }

    /// Forgets the clicks and presses under way (a reset, a state loaded).
    pub fn reset(&mut self) {
        self.pad.clear();
        self.task = None;
        self.clicks.clear();
        self.game.reset();
        self.watched = 0;
    }

    /// Takes the rules' hooks out of the console (mouse switched off).
    pub fn remove(mut self, psx: &mut Psx) {
        self.game.remove(psx);
    }
}

/// A rectangle (x, y, width, height).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    /// How far a point is outside, per axis (0 inside), signed: negative
    /// when it is left of or above the rectangle.
    pub fn outside(&self, x: i32, y: i32) -> (i32, i32) {
        let d = |p: i32, a: i32, len: i32| if p < a { p - a } else if p >= a + len { p - (a + len - 1) } else { 0 };
        (d(x, self.x, self.w), d(y, self.y, self.h))
    }
}

/// Moves a cursor onto a point with the D-pad, one press at a time, then
/// confirms. `cursor` reports where the cursor is now (`None`: the menu went
/// away). Rows first, then columns. A press that doesn't bring the cursor
/// closer ends it (the end of a list, or a jump past it, which is undone);
/// nothing is confirmed unless the cursor got onto the point.
pub(crate) fn steer<F>(x: i32, y: i32, confirm: u16, cursor: F) -> Task
where
    F: Fn(&Psx) -> Option<Rect> + Send + 'static,
{
    // The press made last and where the cursor was before it.
    let mut last: Option<(u16, Rect)> = None;
    let mut blocked = 0u16;
    let mut presses = 0;
    Box::new(move |psx, pad| {
        let Some(r) = cursor(psx) else { return false };
        let (dx, dy) = r.outside(x, y);
        if let Some((dir, before)) = last.take() {
            let (bx, by) = before.outside(x, y);
            if r == before {
                // It didn't move that way: try the other axis.
                blocked |= dir;
            } else if dx.abs() + dy.abs() > bx.abs() + by.abs() || (dir & (pad::UP | pad::DOWN) != 0 && dy.signum() == -by.signum() && dy != 0) {
                // It jumped the wrong way (wrapped round): back, and stop.
                pad.tap(opposite(dir));
                return false;
            }
        }
        if dx == 0 && dy == 0 {
            pad.tap(confirm);
            return false;
        }
        presses += 1;
        if presses > 40 {
            return false;
        }
        let want = [(dy < 0, pad::UP), (dy > 0, pad::DOWN), (dx < 0, pad::LEFT), (dx > 0, pad::RIGHT)];
        let Some(&(_, dir)) = want.iter().find(|(w, d)| *w && blocked & d == 0) else { return false };
        pad.tap(dir);
        last = Some((dir, r));
        true
    })
}

fn opposite(dir: u16) -> u16 {
    match dir {
        pad::UP => pad::DOWN,
        pad::DOWN => pad::UP,
        pad::LEFT => pad::RIGHT,
        _ => pad::LEFT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn console() -> Psx {
        Psx::new(&vec![0u8; 512 * 1024], dwrc_psx::VideoStandard::Ntsc)
    }

    /// The game's copy of the pad, in RAM for the tests.
    const SEEN: u32 = 0x8000_1000;

    #[test]
    fn a_press_is_held_until_the_game_saw_it() {
        let psx = console();
        let mut pad = Pad::new(Some(Box::new(|bus: &Bus| bus.peek16(SEEN))));
        let mut bus = psx.bus;
        pad.tap(pad::CROSS);
        assert_eq!(pad.held(), pad::CROSS);
        // The game reads the pad only every few frames: still held.
        pad.tick(&bus);
        pad.tick(&bus);
        assert_eq!(pad.held(), pad::CROSS);
        bus.poke16(SEEN, pad::CROSS);
        pad.tick(&bus);
        // Released until the game has seen that too.
        assert_eq!(pad.held(), 0);
        pad.tick(&bus);
        assert!(pad.busy());
        bus.poke16(SEEN, 0);
        pad.tick(&bus);
        assert!(!pad.busy());
        // Without the game's copy, a few frames each.
        let mut blind = Pad::new(None);
        blind.tap(pad::UP);
        for _ in 0..BLIND_FRAMES * 2 {
            assert!(blind.busy());
            blind.held();
            blind.tick(&bus);
        }
        assert!(!blind.busy());
    }

    /// Runs `steer` against a list of `rows` options 10 high from y 0 that
    /// wraps round at its ends, the cursor in RAM; returns where the cursor
    /// ended and whether it confirmed.
    fn steer_list(start: u8, rows: u8, x: i32, y: i32) -> (u8, bool) {
        const POS: u32 = 0x8000_2000;
        let mut psx = console();
        psx.bus.poke8(POS, start);
        let mut task = steer(x, y, pad::CROSS, |psx| Some(Rect::new(0, psx.bus.peek8(POS) as i32 * 10, 50, 10)));
        let mut pad = Pad::new(None);
        let mut confirmed = false;
        for _ in 0..100 {
            let go_on = task(&mut psx, &mut pad);
            while let Some(step) = pad.steps.pop_front() {
                let pos = psx.bus.peek8(POS);
                match step {
                    Step::Tap(pad::DOWN) => psx.bus.poke8(POS, (pos + 1) % rows),
                    Step::Tap(pad::UP) => psx.bus.poke8(POS, (pos + rows - 1) % rows),
                    Step::Tap(pad::CROSS) => confirmed = true,
                    _ => {}
                }
            }
            if !go_on {
                break;
            }
        }
        (psx.bus.peek8(POS), confirmed)
    }

    #[test]
    fn steering_onto_an_option() {
        assert_eq!(steer_list(0, 5, 20, 35), (3, true));
        assert_eq!(steer_list(4, 5, 20, 5), (0, true));
        assert_eq!(steer_list(2, 5, 20, 25), (2, true));
        // Below the list: it stops at the end (undoing the wrap) and doesn't confirm.
        assert_eq!(steer_list(1, 5, 20, 80), (4, false));
        // Beside the option: left and right don't move it, nothing confirmed.
        assert_eq!(steer_list(1, 5, 70, 15), (1, false));
    }

    #[test]
    fn rect_outside() {
        let r = Rect::new(10, 20, 30, 10);
        assert_eq!(r.outside(15, 25), (0, 0));
        assert_eq!(r.outside(5, 25), (-5, 0));
        assert_eq!(r.outside(45, 35), (6, 6));
        assert!(r.contains(10, 20) && !r.contains(40, 20));
    }
}
