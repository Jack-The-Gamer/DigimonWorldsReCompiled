//! Digimon World 1: the mouse rules. Addresses are from the dw_decomp
//! decompilation (`config/<version>/symbols*.txt`; the USA, European and
//! Japanese rev 1 executables match its builds byte for byte), names as
//! there. The game keeps its UI as objects in `WORLD_OBJECTS` (id, instance,
//! tick, render), which says what is on screen; its UI coordinates have
//! (0, 0) in the middle of the 320×240 picture.
//!
//! * Field and dialogue: left presses confirm, right the menu button.
//! * The title's "Press Start Button" and movies: left presses Start.
//! * Lists with the selection cursor (`renderSelectionCursor`: the
//!   inventory and its boxes, dialogue choices, shops) or the title screen's
//!   highlight (`renderMenuSelector`: the title and memory card menus): left
//!   steers the cursor onto the option and confirms, right cancels.
//! * The Triangle menu: the icon clicked is selected and confirmed.
//! * The Digimon and Player menus: their tabs switch to that tab; the Tech
//!   tab's grid equips the technique clicked; the Player menu's evolution
//!   chart, medals and cards show the one clicked.
//! * Battle (also tournaments and Vs battles): a command's icon is selected
//!   and confirmed; the finisher gauge (once full) starts the finisher; while
//!   a finisher charges, every click is an L1 or R1 press; right opens the
//!   item menu.
//! * Vs mode's mode and arena choices: the one clicked is taken.
//! * Training: right ends it early (cancel held).

use super::{pad, steer, Click, Game, Input, Pad, Rect, Task};
use dwrc_disc::Identification;
use dwrc_psx::{Bus, HookAction, HookId, Psx};
use std::sync::{Arc, Mutex};

/// One executable's addresses.
struct Addrs {
    serial: &'static str,
    sha1: &'static str,
    polled_input: u32,
    world_objects: u32,
    game_state: u32,
    combat_data_ptr: u32,
    ui_box_data: u32,
    is_in_menu: u32,
    menu_options: u32,
    menu_option_count: u32,
    triangle_menu_state: u32,
    menu_state: u32,
    digimon_menu_state: u32,
    player_menu_state: u32,
    move_select_box_x: u32,
    move_select_box_y: u32,
    flee_disabled: u32,
    tick_game_menu: u32,
    tick_digimon_menu: u32,
    tick_player_menu: u32,
    render_selection_cursor: u32,
    /// The title screen's and memory card menus' highlight (main_menu.c).
    render_menu_selector: u32,
    /// The title's "Press Start Button" (object 0xFA3's render).
    render_press_start: u32,
    /// `BTL_COMMAND_MENU_X` and `_Y` (the battle overlay).
    btl_command_menu_x: u32,
    btl_command_menu_y: u32,
    /// The Player menu's grids: `CHART_SELECTED_COLUMN`, `_ROW` (s16),
    /// `EVO_CHART_BOXES`, `MEDAL_SELECTOR_INDEX`, `SELECTED_CARD` (u8).
    chart_column: u32,
    chart_row: u32,
    evo_chart_boxes: u32,
    medal_selector: u32,
    selected_card: u32,
    /// Tournament (STD) and Vs battles: their command menus' render functions
    /// (object 0x198 in each overlay) and `*_COMMAND_MENU_TOP`, `_BOTTOM`.
    std_render_command_menu: u32,
    vs_render_command_menu: u32,
    std_command_menu_top: u32,
    std_command_menu_bottom: u32,
    vs_command_menu_top: u32,
    vs_command_menu_bottom: u32,
    /// Vs mode's mode and arena boxes: `VS__tickSelectBox`,
    /// `VS__SELECT_BOX_DATA_SELECTION`, `_SELECTION_COUNT`.
    vs_tick_select_box: u32,
    vs_selection: u32,
    vs_selection_count: u32,
    /// The Europe version's menu button is Circle (`tamerTickWalkingState`).
    menu_button: u16,
    /// Japan: confirm Circle, cancel Cross (`include/dw/input.h`).
    japan: bool,
}

const VERSIONS: [Addrs; 3] = [
    Addrs {
        serial: "SLUS-01032",
        sha1: "31ec666b987d5b007dec40eecc4092d27ed9085b",
        polled_input: 0x80134EE4,
        world_objects: 0x80137418,
        game_state: 0x80134F0A,
        combat_data_ptr: 0x80134D4C,
        ui_box_data: 0x8013D390,
        is_in_menu: 0x80134EF0,
        menu_options: 0x80123E88,
        menu_option_count: 0x80134D28,
        triangle_menu_state: 0x80134D30,
        menu_state: 0x80134D34,
        digimon_menu_state: 0x80134D36,
        player_menu_state: 0x80134D37,
        move_select_box_x: 0x80134D3A,
        move_select_box_y: 0x80134D38,
        flee_disabled: 0x80134D7C,
        tick_game_menu: 0x800B8BF0,
        tick_digimon_menu: 0x800B8F54,
        tick_player_menu: 0x800B94D4,
        render_selection_cursor: 0x800E5A3C,
        render_menu_selector: 0x801136C8,
        render_press_start: 0x800EFEBC,
        btl_command_menu_x: 0x80135084,
        btl_command_menu_y: 0x80135086,
        chart_column: 0x80134D42,
        chart_row: 0x80134D44,
        evo_chart_boxes: 0x80124544,
        medal_selector: 0x80134D3D,
        selected_card: 0x80134D3E,
        std_render_command_menu: 0x8006887C,
        vs_render_command_menu: 0x8005FD20,
        std_command_menu_top: 0x8013517C,
        std_command_menu_bottom: 0x80135180,
        vs_command_menu_top: 0x801352AC,
        vs_command_menu_bottom: 0x801352B0,
        vs_tick_select_box: 0x800FA088,
        vs_selection: 0x80134F59,
        vs_selection_count: 0x80134F58,
        menu_button: pad::TRIANGLE,
        japan: false,
    },
    Addrs {
        serial: "SLES-02914",
        sha1: "a805aad8bb6b2a12b145bd2172697aff7a28d25e",
        polled_input: 0x8013C0BC,
        world_objects: 0x8013E3F8,
        game_state: 0x8013C0DA,
        combat_data_ptr: 0x8013BE1C,
        ui_box_data: 0x80144958,
        is_in_menu: 0x8013C0C0,
        menu_options: 0x8012BAE4,
        menu_option_count: 0x8013BE00,
        triangle_menu_state: 0x8013BDF8,
        menu_state: 0x8013BE06,
        digimon_menu_state: 0x8013BE07,
        player_menu_state: 0x8013BE04,
        move_select_box_x: 0x8013BE0A,
        move_select_box_y: 0x8013BE08,
        flee_disabled: 0x8013BE4C,
        tick_game_menu: 0x800BF4DC,
        tick_digimon_menu: 0x800BEFB4,
        tick_player_menu: 0x800BE060,
        render_selection_cursor: 0x801011A0,
        render_menu_selector: 0x8010E5C0,
        render_press_start: 0x801164F8,
        btl_command_menu_x: 0x8013C108,
        btl_command_menu_y: 0x8013C106,
        chart_column: 0x8013BE14,
        chart_row: 0x8013BE12,
        evo_chart_boxes: 0x8012C100,
        medal_selector: 0x8013BE10,
        selected_card: 0x8013BE0E,
        std_render_command_menu: 0x8006CA14,
        vs_render_command_menu: 0x8006CFB8,
        std_command_menu_top: 0x8013C208,
        std_command_menu_bottom: 0x8013C204,
        vs_command_menu_top: 0x8013C37C,
        vs_command_menu_bottom: 0x8013C378,
        vs_tick_select_box: 0x8005A03C,
        vs_selection: 0x8013C2E9,
        vs_selection_count: 0x8013C2EC,
        menu_button: pad::CIRCLE,
        japan: false,
    },
    Addrs {
        serial: "SLPS-01797",
        sha1: "82212496037146885e96573bd43aa3f0f4ccee8e",
        polled_input: 0x8013ECE0,
        world_objects: 0x8014111C,
        game_state: 0x8013ED06,
        combat_data_ptr: 0x8013EA40,
        ui_box_data: 0x8014767C,
        is_in_menu: 0x8013ECEC,
        menu_options: 0x80130708,
        menu_option_count: 0x8013EA1C,
        triangle_menu_state: 0x8013EA24,
        menu_state: 0x8013EA28,
        digimon_menu_state: 0x8013EA2A,
        player_menu_state: 0x8013EA2B,
        move_select_box_x: 0x8013EA2C,
        move_select_box_y: 0x8013EA2E,
        flee_disabled: 0x8013EA70,
        tick_game_menu: 0x800BF2C0,
        tick_digimon_menu: 0x800BF718,
        tick_player_menu: 0x800BFD48,
        render_selection_cursor: 0x801040E4,
        render_menu_selector: 0x80111428,
        render_press_start: 0x80118650,
        btl_command_menu_x: 0x8013ED28,
        btl_command_menu_y: 0x8013ED2A,
        chart_column: 0x8013EA36,
        chart_row: 0x8013EA38,
        evo_chart_boxes: 0x80130DC4,
        medal_selector: 0x8013EA31,
        selected_card: 0x8013EA32,
        std_render_command_menu: 0x8006CFE0,
        vs_render_command_menu: 0x8006D308,
        std_command_menu_top: 0x8013EE20,
        std_command_menu_bottom: 0x8013EE24,
        vs_command_menu_top: 0x8013EF90,
        vs_command_menu_bottom: 0x8013EF94,
        vs_tick_select_box: 0x80059F48,
        vs_selection: 0x8013EF05,
        vs_selection_count: 0x8013EF04,
        menu_button: pad::TRIANGLE,
        japan: true,
    },
];

/// `WORLD_OBJECTS` entries (`NUM_WORLD_OBJECTS`) and the object ids used.
const NUM_WORLD_OBJECTS: u32 = 128;
const OBJ_TRIANGLE_MENU: i16 = 0xFA4;
const OBJ_BATTLE_COMMANDS: i16 = 0x198;
const OBJ_FINISHER_CHARGEUP: i16 = 0x19A;
/// The training objects (`TRN_tick*Training`, 0xFAB..0xFB0).
const OBJ_TRAINING: std::ops::RangeInclusive<i16> = 0xFAB..=0xFB0;

/// `CombatData` fields (include/dw/combat.h): `fighter[0]` first, then `player`.
const FINISHER_GOAL: u32 = 0x18;
const FINISHER_PROGRESS: u32 = 0x1A;
const CURRENT_COMMAND: u32 = 0x5A0 + 0xAE;
const HOVERED_COMMAND: u32 = 0x5A0 + 0xB2;
const NUM_COMMANDS: u32 = 0x5A0 + 0xC6;
/// The finisher's gauge and "FINISH" sign: in battle (BTL_renderFinisher-
/// Gauge, BTL_renderFinisherReadyIcon), and the player's, on the left, in
/// tournaments and Vs battles (STD_renderFinisherReadyIcon at (-0x58, -0x4C)).
const BTL_FINISHER: Rect = Rect::new(0x34, -0x50, 0x64, 0x12);
const STD_FINISHER: Rect = Rect::new(-0x5C, -0x50, 0x48, 0x14);
/// The finisher command (`currentCommand` 0xB).
const COMMAND_FINISHER: u8 = 0xB;

/// `UIBoxData` (include/dw/ui.h): 0x24 bytes; finalPos at 8, state at 0x12, tick at 0x1C.
const UI_BOX_SIZE: u32 = 0x24;

/// A selection cursor drawn (`renderSelectionCursor(x, y, w, h, layer)`).
#[derive(Clone, Copy)]
struct Drawn {
    vsync: u64,
    rect: Rect,
    layer: i32,
}

/// The selection cursors drawn lately, newest last.
type CursorLog = Arc<Mutex<Vec<Drawn>>>;

/// A cursor counts as on screen for this many vsyncs after it was drawn
/// (the game draws a picture every 2-3 vsyncs, slower when busy).
const CURSOR_FRESH: u64 = 9;

pub(super) fn new(psx: &mut Psx, ident: &Identification) -> Option<(Box<dyn Game>, Option<Input>)> {
    let a = VERSIONS.iter().find(|a| a.serial == ident.serial && (ident.exe_sha1.is_empty() || ident.exe_sha1 == a.sha1));
    let Some(a) = a else {
        log::info!("mouse: no rules for {} (executable {})", ident.serial, ident.exe_sha1);
        return None;
    };
    let log: CursorLog = Arc::new(Mutex::new(Vec::new()));
    let note = |l: &CursorLog, bus: &Bus, rect: Rect, layer: i32| {
        let mut l = l.lock().unwrap();
        let vsync = bus.gpu.frames;
        l.retain(|d| d.vsync <= vsync && d.vsync + CURSOR_FRESH >= vsync);
        l.push(Drawn { vsync, rect, layer });
    };
    let l = log.clone();
    let selection = psx.bus.add_hook(
        a.render_selection_cursor,
        Box::new(move |cpu, bus| {
            let r = &cpu.regs;
            let layer = bus.peek32(r[29].wrapping_add(16)) as i32;
            note(&l, bus, Rect::new(r[4] as i32, r[5] as i32, r[6] as i16 as i32, r[7] as i16 as i32), layer);
            HookAction::Continue
        }),
    );
    // renderMenuSelector(MenuHighlight *): {pos, pad[5], rowHeight, x, y, w,
    // h}, a frame around row `pos` in screen pixels, on the front layer.
    let l = log.clone();
    let highlight = psx.bus.add_hook(
        a.render_menu_selector,
        Box::new(move |cpu, bus| {
            let b = cpu.regs[4];
            let h = |o: u32| bus.peek16(b + o) as i16 as i32;
            let y = h(10) + h(6) * bus.peek8(b) as i8 as i32;
            note(&l, bus, Rect::new(h(8) - 1 - 160, y - 2 - 120, h(12) + 2, h(14) + 3), 0);
            HookAction::Continue
        }),
    );
    let (confirm, cancel) = if a.japan { (pad::CIRCLE, pad::CROSS) } else { (pad::CROSS, pad::TRIANGLE) };
    let game = Dw1 { a, log, hooks: vec![selection, highlight], confirm, cancel, l1_next: true };
    let polled = a.polled_input;
    Some((Box::new(game), Some(Box::new(move |bus: &Bus| bus.peek32(polled) as u16))))
}

struct Dw1 {
    a: &'static Addrs,
    log: CursorLog,
    hooks: Vec<HookId>,
    confirm: u16,
    cancel: u16,
    /// The next finisher charge-up press: L1 (else R1).
    l1_next: bool,
}

/// The front-most selection cursor of the newest picture that has one.
fn cursor(log: &CursorLog, bus: &Bus) -> Option<Rect> {
    let l = log.lock().unwrap();
    let now = bus.gpu.frames;
    let newest = l.iter().rev().find(|d| d.vsync <= now && d.vsync + CURSOR_FRESH >= now)?.vsync;
    // Pictures take a few vsyncs: the cursors of the last one drawn.
    let pic: Vec<&Drawn> = l.iter().filter(|d| d.vsync + 2 >= newest && d.vsync <= newest).collect();
    let front = pic.iter().map(|d| d.layer).min()?;
    pic.iter().rev().find(|d| d.layer == front).map(|d| d.rect)
}

/// The objects in `WORLD_OBJECTS`: (id, render function).
fn objects(bus: &Bus, a: &Addrs) -> Vec<(i16, u32)> {
    (0..NUM_WORLD_OBJECTS)
        .map(|i| a.world_objects + i * 12)
        .map(|o| (bus.peek16(o) as i16, bus.peek32(o + 8)))
        .take_while(|&(id, _)| id != -1)
        .collect()
}

struct UiBox {
    state: i16,
    tick: u32,
}

fn ui_box(bus: &Bus, a: &Addrs, id: u32) -> UiBox {
    let b = a.ui_box_data + id * UI_BOX_SIZE;
    UiBox { state: bus.peek16(b + 0x12) as i16, tick: bus.peek32(b + 0x1C) }
}

/// The tabs of the Digimon and Player menus (`renderMenuTab(x, w)`, at y -0x68, 16 high).
const DIGIMON_TABS: [(i32, i32); 2] = [(-0x91, 0x4C), (-0x46, 0x40)];
const PLAYER_TABS: [(i32, i32); 4] = [(-0x91, 0x4C), (-0x46, 0x4C), (5, 0x34), (0x38, 0x34)];

fn tab_at(tabs: &[(i32, i32)], x: i32, y: i32) -> Option<usize> {
    tabs.iter().position(|&(tx, w)| Rect::new(tx, -0x68, w, 0x10).contains(x, y))
}

impl Dw1 {
    /// A click on a battle screen (the command icons, the finisher). `render`
    /// is the command menu's render function, which says whose battle it is:
    /// a tournament's (STD) and Vs mode's draw the bar the same way from their
    /// own top and bottom, with the player's finisher sign on the left.
    fn battle(&mut self, bus: &mut Bus, render: u32, x: i32, y: i32, pad: &mut Pad) {
        let a = self.a;
        let cd = bus.peek32(a.combat_data_ptr);
        if cd & 0x1F_FFFF == 0 {
            return;
        }
        let (top, bottom, finisher, flee) = if render == a.std_render_command_menu {
            (a.std_command_menu_top, a.std_command_menu_bottom, STD_FINISHER, false)
        } else if render == a.vs_render_command_menu {
            (a.vs_command_menu_top, a.vs_command_menu_bottom, STD_FINISHER, false)
        } else {
            (a.btl_command_menu_x, a.btl_command_menu_y, BTL_FINISHER, true)
        };
        // The finisher gauge and its "FINISH" sign: Square once the gauge is full.
        let full = bus.peek16(cd + FINISHER_PROGRESS) == bus.peek16(cd + FINISHER_GOAL);
        if finisher.contains(x, y) {
            if full && bus.peek8(cd + CURRENT_COMMAND) != COMMAND_FINISHER {
                pad.tap(pad::SQUARE);
            }
            return;
        }
        // The command icons (BTL_renderCommandMenu): right to left from
        // `base`, every other one lower, 16×16, where the menu has slid to.
        let num = bus.peek8(cd + NUM_COMMANDS) as i32;
        let (mx, my) = (bus.peek16(top) as i16 as i32, bus.peek16(bottom) as i16 as i32);
        let base = (num - 1) * 0xE - 0x8C;
        let count = num - 1;
        let flee_disabled = flee && bus.peek8(a.flee_disabled) != 0;
        let mut best: Option<(i32, i32)> = None;
        for i in 1..num.min(9) {
            if i == 1 && flee_disabled {
                continue;
            }
            let high = (i % 2 == 0) == (count % 2 == 0);
            let (top, bottom) = if high { (mx + 3, my - 0xD) } else { (mx + 0xD, my - 3) };
            let r = Rect::new(base - i * 0xE - 1, top - 1, 0x12, bottom - top + 2);
            if r.contains(x, y) {
                let d = (x - (r.x + r.w / 2)).pow(2) + (y - (r.y + r.h / 2)).pow(2);
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((i, d));
                }
            }
        }
        if let Some((i, _)) = best {
            bus.poke8(cd + HOVERED_COMMAND, i as u8);
            pad.tap(self.confirm);
        }
    }

    /// A click on the Triangle menu: selects the option and confirms (tickGameMenu).
    fn triangle_menu(&self, bus: &mut Bus, x: i32, y: i32, pad: &mut Pad) {
        let a = self.a;
        let count = bus.peek32(a.menu_option_count) as i32;
        // renderGameMenu: without the fishing rod the icons are 0x28 higher.
        let y_offset = if count == 7 { -0x28 } else { 0 };
        for i in 1..count.clamp(1, 8) {
            let o = a.menu_options + i as u32 * 12;
            let (ox, oy) = (bus.peek16(o) as i16 as i32 - 0x42, bus.peek16(o + 2) as i16 as i32 - 0x50 + y_offset);
            let (w, h) = (bus.peek8(o + 8) as i32, bus.peek8(o + 9) as i32);
            // The icon and its label below it.
            if Rect::new(ox - 6, oy - 3, w + 12, h + 16).contains(x, y) {
                bus.poke16(a.menu_options + 4, i as u16);
                pad.tap(self.confirm);
                return;
            }
        }
    }

    /// Switches the Digimon or Player menu to tab `to`: Right or Left until
    /// it is there (the menus take them only once a tab has settled, in
    /// MENU_STATE 1).
    fn switch_tab(&self, tab_addr: u32, to: usize) -> Task {
        let a = self.a;
        let menu_state = a.menu_state;
        let cancel = self.cancel;
        let mut tries = 0;
        Box::new(move |psx, pad| {
            let now = psx.bus.peek8(tab_addr) as i8 as i32;
            tries += 1;
            if now == to as i32 || tries > 120 {
                return false;
            }
            match psx.bus.peek8(menu_state) as i8 {
                1 => pad.tap(if (to as i32) > now { pad::RIGHT } else { pad::LEFT }),
                // In a tab's grid or its detail: back out to the tabs first.
                2..=6 => pad.tap(cancel),
                _ => pad.wait(1),
            }
            true
        })
    }

    /// A click on one of the Player menu's grids (tickPlayerMenu): into the
    /// grid if not there yet (confirm, MENU_STATE 2), out of the detail view
    /// if one is open (MENU_STATE 3, cancel; `detail` is where it shows, and
    /// a click on it only closes it), then `set` picks the cell and, with
    /// `open`, confirm shows it.
    fn grid(&self, x: i32, y: i32, detail: Option<Rect>, open: bool, set: impl Fn(&mut Bus) + Send + 'static) -> Task {
        let a = self.a;
        let (confirm, cancel) = (self.confirm, self.cancel);
        let mut waited = 0;
        Box::new(move |psx: &mut Psx, pad: &mut Pad| match psx.bus.peek8(a.menu_state) as i8 {
            1 => {
                pad.tap(confirm);
                true
            }
            2 => {
                set(&mut psx.bus);
                if open {
                    pad.wait(2);
                    pad.tap(confirm);
                }
                false
            }
            3 if detail.is_some() => {
                pad.tap(cancel);
                !detail.is_some_and(|d| d.contains(x, y))
            }
            _ => {
                waited += 1;
                pad.wait(1);
                waited < 90
            }
        })
    }

    /// The Player menu's tabs' grids: the evolution chart (tab 1), medals
    /// (2) and cards (3); UI coordinates `(x, y)`, screen pixels `(sx, sy)`.
    fn player_grid(&self, bus: &Bus, x: i32, y: i32, sx: i32, sy: i32) -> Option<Task> {
        let a = self.a;
        match bus.peek8(a.player_menu_state) {
            1 => {
                // renderEvoChartView: EVO_CHART_BOXES {posX, posY, ...}, 8
                // bytes, 16×16 boxes in screen pixels; the selector's column
                // and row are worked back from a box's place (tickPlayerMenu).
                let (bx, by) = (0..61u32)
                    .map(|i| a.evo_chart_boxes + i * 8)
                    .map(|e| (bus.peek16(e) as i16 as i32, bus.peek16(e + 2) as i16 as i32))
                    .find(|&(bx, by)| Rect::new(bx - 1, by - 1, 0x12, 0x12).contains(sx, sy))?;
                let (col, row) = chart_cell(bx, by);
                // The detail box (renderEvoChartDetail) covers the chart.
                Some(self.grid(x, y, Some(Rect::new(-0x96, -0x59, 0x12C, 0xBE)), true, move |bus| {
                    bus.poke16(a.chart_column, col as u16);
                    bus.poke16(a.chart_row, row as u16);
                }))
            }
            2 => {
                // renderMedalView: 5 columns 38 apart, 3 rows 24 apart; the
                // medal picked is shown at once.
                let i = (0..15).find(|i| Rect::new(i % 5 * 38 - 0x82, i / 5 * 24 - 0x3D, 32, 24).contains(x, y))?;
                Some(self.grid(x, y, None, false, move |bus| bus.poke8(a.medal_selector, i as u8)))
            }
            3 => {
                // renderCardsView: 11 columns and 6 rows, 24 apart; confirm
                // shows the card (renderCardImage) if there is one.
                let i = (0..66).find(|i| Rect::new(i % 11 * 24 - 0x84, i / 11 * 24 - 0x40, 24, 24).contains(x, y))?;
                Some(self.grid(x, y, Some(Rect::new(-0x4B, -0x53, 0x96, 0xB4)), true, move |bus| bus.poke8(a.selected_card, i as u8)))
            }
            _ => None,
        }
    }

    /// The Tech tab's grid (renderDigimonMovesView, MENU_STATE 6): 8 columns
    /// 18 wide from x 0x73, 7 rows 15 high from y 0x6F (screen pixels).
    fn tech_grid(&self, sx: i32, sy: i32) -> Option<Task> {
        let (col, row) = ((sx - 0x73).div_euclid(0x12), (sy - 0x6F).div_euclid(0xF));
        if !(0..8).contains(&col) || !(0..7).contains(&row) {
            return None;
        }
        let a = self.a;
        let confirm = self.confirm;
        let mut waited = 0;
        Some(Box::new(move |psx: &mut Psx, pad: &mut Pad| {
            let st = psx.bus.peek8(a.menu_state) as i8;
            match st {
                // Not picking yet: confirm starts it.
                1 => {
                    pad.tap(confirm);
                    true
                }
                6 => {
                    psx.bus.poke16(a.move_select_box_x, (0x73 + col * 0x12) as u16);
                    psx.bus.poke16(a.move_select_box_y, (0x6F + row * 0xF) as u16);
                    pad.tap(confirm);
                    false
                }
                _ => {
                    waited += 1;
                    pad.wait(1);
                    waited < 90
                }
            }
        }))
    }
}

impl Game for Dw1 {
    fn click(&mut self, psx: &mut Psx, c: Click, pad: &mut Pad) -> Option<Task> {
        let a = self.a;
        // Screen pixels (320×240) and the game's UI coordinates.
        let (sx, sy) = (c.x * 320 / c.width.max(1), c.y * 240 / c.height.max(1));
        let (x, y) = (sx - 160, sy - 120);
        let bus = &mut psx.bus;
        let objs = objects(bus, a);
        let has = |id: i16| objs.iter().any(|o| o.0 == id);

        // The title's "Press Start Button", and movies (playMovie runs on its
        // own, only the always-there object 0x404 left; it reads the pad
        // itself and stops at Start).
        if !c.right && (objs.iter().any(|o| o.1 == a.render_press_start) || objs.iter().all(|o| o.0 == 0x404)) {
            pad.tap(pad::START);
            return None;
        }
        if objs.iter().any(|o| OBJ_TRAINING.contains(&o.0)) {
            // Training: cancel held ends it early (TRN_tick*Training read it held).
            if c.right {
                pad.hold(self.cancel, 6);
            } else {
                pad.tap(self.confirm);
            }
            return None;
        }
        // 1: a battle; 4: a tournament's or Vs mode's fight.
        let battle = matches!(bus.peek8(a.game_state), 1 | 4);
        let cd = bus.peek32(a.combat_data_ptr);
        let finishing = cd & 0x1F_FFFF != 0 && bus.peek8(cd + CURRENT_COMMAND) == COMMAND_FINISHER;
        if battle && (finishing || has(OBJ_FINISHER_CHARGEUP)) {
            // BTL_tickFinisherChargeup: each new L1 or R1 press charges it.
            // From the finisher's order on (the charge-up opens a moment
            // later), so that mashing both buttons opens no menu; L1 and R1
            // do nothing else in battle.
            pad.tap(if self.l1_next { pad::L1 } else { pad::R1 });
            self.l1_next = !self.l1_next;
            return None;
        }
        if let Some(r) = cursor(&self.log, bus) {
            log::debug!("mouse: click ({x}, {y}) on a list, cursor at {r:?}");
            if c.right {
                pad.tap(self.cancel);
                return None;
            }
            let log = self.log.clone();
            return Some(steer(x, y, self.confirm, move |psx| cursor(&log, &psx.bus)));
        }
        if let Some(&(_, render)) = objs.iter().find(|o| o.0 == OBJ_BATTLE_COMMANDS).filter(|_| battle) {
            log::debug!("mouse: click ({x}, {y}) in battle");
            if c.right {
                // tamerTickBattle: the item menu.
                pad.tap(if a.serial == "SLUS-01032" { self.cancel } else { a.menu_button });
            } else {
                self.battle(bus, render, x, y, pad);
            }
            return None;
        }
        let (box0, box1) = (ui_box(bus, a, 0), ui_box(bus, a, 1));
        // Vs mode's mode (box 0) and arena (box 1) choices (VS__tickSelectBox):
        // three 56 apart, the chosen one at VS__SELECT_BOX_DATA_SELECTION (the
        // modes up to _SELECTION_COUNT); confirm takes it.
        let vs = [&box0, &box1].iter().position(|b| b.tick == a.vs_tick_select_box && b.state == 1);
        if let Some(id) = vs.filter(|_| !c.right) {
            let last = if id == 0 { bus.peek8(a.vs_selection_count) as i32 } else { 2 };
            if let Some(i) = (0..=last.min(2)).find(|i| Rect::new(-0x52 + i * 56, -0x33, 0x34, 0x64).contains(x, y)) {
                bus.poke8(a.vs_selection, i as u8);
                pad.wait(4);
            }
            pad.tap(self.confirm);
            return None;
        }
        if has(OBJ_TRIANGLE_MENU) && box0.state == 1 && box0.tick == a.tick_game_menu && bus.peek32(a.triangle_menu_state) as i32 == -1 {
            if c.right {
                pad.tap(self.cancel);
            } else {
                self.triangle_menu(bus, x, y, pad);
            }
            return None;
        }
        if box1.state == 1 && (box1.tick == a.tick_digimon_menu || box1.tick == a.tick_player_menu) {
            if c.right {
                pad.tap(self.cancel);
                return None;
            }
            let digimon = box1.tick == a.tick_digimon_menu;
            let (tabs, addr): (&[(i32, i32)], u32) = if digimon { (&DIGIMON_TABS, a.digimon_menu_state) } else { (&PLAYER_TABS, a.player_menu_state) };
            if let Some(t) = tab_at(tabs, x, y) {
                return Some(self.switch_tab(addr, t));
            }
            if digimon && bus.peek8(a.digimon_menu_state) == 1 {
                return self.tech_grid(sx, sy);
            }
            if !digimon {
                return self.player_grid(bus, x, y, sx, sy);
            }
            return None;
        }
        // The field, dialogue, and screens without rules of their own.
        let in_menu = bus.peek32(a.is_in_menu) != 0 || (0..4).any(|i| ui_box(bus, a, i).state != 0);
        pad.tap(match (c.right, in_menu) {
            (false, _) => self.confirm,
            (true, true) => self.cancel,
            (true, false) => a.menu_button,
        });
        None
    }

    fn reset(&mut self) {
        self.log.lock().unwrap().clear();
    }

    fn remove(&mut self, psx: &mut Psx) {
        for h in self.hooks.drain(..) {
            psx.bus.remove_hook(h);
        }
    }
}

/// The evolution chart's selector column and row (tickPlayerMenu) on the box
/// at `(bx, by)`: the selector's x is `col * 0x25 + 0x1C` for the first three
/// columns, then 0x18 apart from 0x8B and from 0xF8, 2 left of its box; rows
/// 0x13 apart from y 0x2B.
fn chart_cell(bx: i32, by: i32) -> (i32, i32) {
    let sel = bx - 2;
    let col = if sel < 0x8B {
        (sel - 0x1C) / 0x25
    } else if sel < 0xF8 {
        (sel - 0x8B) / 0x18 + 3
    } else {
        (sel - 0xF8) / 0x18 + 7
    };
    (col, (by - 0x2B) / 0x13)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_cells() {
        // The game's own selector place for a column (tickPlayerMenu).
        let selector = |col: i32| match col {
            0..=2 => col * 0x25 + 0x1C,
            3..=6 => (col - 3) * 0x18 + 0x8B,
            _ => (col - 7) * 0x18 + 0xF8,
        };
        for col in 0..9 {
            for row in 0..9 {
                assert_eq!(chart_cell(selector(col) + 2, row * 0x13 + 0x2B), (col, row));
            }
        }
        // Boxes from EVO_CHART_BOXES: Botamon, Agumon, Greymon, MetalGreymon.
        assert_eq!(chart_cell(0x1E, 0x2B), (0, 0));
        assert_eq!(chart_cell(0x68, 0x2B), (2, 0));
        assert_eq!(chart_cell(0x8D, 0x2B), (3, 0));
        assert_eq!(chart_cell(0xFA, 0x2B), (7, 0));
    }
}
