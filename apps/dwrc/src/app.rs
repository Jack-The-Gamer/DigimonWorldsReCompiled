//! The launcher / player user interface.

use crate::audio::{self, AudioOut, AudioShared};
use crate::emu::{EmuCmd, EmuHandle};
use crate::input::{self, Input};
use dwrc_disc::GameId;
use dwrc_mods::{ModInfo, OptionKind};
use dwrc_psx::Button;
use dwrc_runtime::bios::region_label;
use dwrc_runtime::config::{Aspect, EngineKind, Filter};
use dwrc_runtime::{Config, DiscStatus, Library, Paths};
use eframe::egui::{self, Align, Align2, Color32, Key, Layout, RichText, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(255, 140, 26);
const ACCENT_DIM: Color32 = Color32::from_rgb(120, 70, 20);
const BLUE: Color32 = Color32::from_rgb(64, 156, 255);
const CARD_BG: Color32 = Color32::from_rgb(28, 31, 38);
const PANEL_BG: Color32 = Color32::from_rgb(18, 20, 25);
const OK: Color32 = Color32::from_rgb(90, 200, 120);
const WARN: Color32 = Color32::from_rgb(240, 190, 70);
const BAD: Color32 = Color32::from_rgb(235, 90, 90);
const MUTED: Color32 = Color32::from_rgb(150, 156, 168);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Library,
    Settings,
    Mods(GameId),
    Playing,
}

enum JobResult {
    Library(Library, String),
    Failed(String),
}

struct Job {
    label: String,
    progress: Arc<Mutex<(String, f32)>>,
    rx: Receiver<JobResult>,
}

pub struct App {
    paths: Paths,
    lib: Library,
    cfg: Config,
    screen: Screen,
    emu: Option<EmuHandle>,
    audio: Arc<AudioShared>,
    _audio_out: Option<AudioOut>,
    audio_error: Option<String>,
    input: Input,
    game_tex: Option<TextureHandle>,
    game_size: (u32, u32),
    last_seq: u64,
    thumbs: HashMap<GameId, Option<TextureHandle>>,
    job: Option<Job>,
    toast: Option<(String, Instant, bool)>,
    show_pause: bool,
    show_mods_overlay: bool,
    rebinding: Option<Button>,
    fullscreen: bool,
    mods_cache: HashMap<GameId, Vec<ModInfo>>,
    selected_mod: Option<String>,
    restart_needed: bool,
    autostart: Option<GameId>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, autostart: Option<GameId>) -> Self {
        let paths = Paths::detect();
        let _ = paths.ensure();
        let lib = Library::load(&paths);
        let cfg = Config::load(&paths);
        style(&cc.egui_ctx);
        let audio = AudioShared::new();
        audio.set_volume(if cfg.audio.muted { 0.0 } else { cfg.audio.volume });
        let (audio_out, audio_error) = match audio::start(audio.clone()) {
            Ok(a) => (Some(a), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let fullscreen = cfg.video.start_fullscreen;
        if fullscreen {
            cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        }
        Self {
            paths,
            lib,
            cfg,
            screen: Screen::Library,
            emu: None,
            audio,
            _audio_out: audio_out,
            audio_error,
            input: Input::new(),
            game_tex: None,
            game_size: (0, 0),
            last_seq: 0,
            thumbs: HashMap::new(),
            job: None,
            toast: None,
            show_pause: false,
            show_mods_overlay: false,
            rebinding: None,
            fullscreen,
            mods_cache: HashMap::new(),
            selected_mod: None,
            restart_needed: false,
            autostart,
        }
    }

    fn notify(&mut self, msg: impl Into<String>, error: bool) {
        self.toast = Some((msg.into(), Instant::now(), error));
    }

    fn save_cfg(&mut self) {
        if let Err(e) = self.cfg.save(&self.paths) {
            self.notify(format!("Could not save settings: {e}"), true);
        }
    }

    fn mods_for(&mut self, g: GameId) -> &Vec<ModInfo> {
        let paths = self.paths.clone();
        self.mods_cache.entry(g).or_insert_with(|| dwrc_runtime::available_mods(&paths, g))
    }

    // ------------------------------------------------------------------
    // Game lifecycle
    // ------------------------------------------------------------------

    fn start_game(&mut self, g: GameId) {
        if let Err(e) = dwrc_runtime::session::preflight(&self.lib, g) {
            self.notify(e.to_string(), true);
            return;
        }
        self.stop_game();
        let serial = self.lib.games[&g].ident.serial.clone();
        let mods = dwrc_runtime::enabled_mods(&self.paths, &self.cfg, g, &serial);
        let emu = EmuHandle::start(self.paths.clone(), self.lib.clone(), self.cfg.clone(), g, mods, self.audio.clone());
        self.emu = Some(emu);
        self.game_tex = None;
        self.last_seq = 0;
        self.show_pause = false;
        self.show_mods_overlay = false;
        self.restart_needed = false;
        self.screen = Screen::Playing;
    }

    fn stop_game(&mut self) {
        if let Some(mut emu) = self.emu.take() {
            // Keep the last frame as the library thumbnail.
            let g = emu.game;
            {
                let f = emu.shared.frame.lock().unwrap();
                if f.width > 0 {
                    let dir = self.paths.cache_dir(g);
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = save_png(&dir.join("thumbnail.png"), f.width, f.height, &f.pixels);
                }
            }
            emu.stop();
            self.thumbs.remove(&g);
        }
        self.audio.clear();
    }

    fn thumbnail(&mut self, ctx: &egui::Context, g: GameId) -> Option<TextureHandle> {
        if !self.thumbs.contains_key(&g) {
            let p = self.paths.cache_dir(g).join("thumbnail.png");
            let tex = load_png(&p).map(|(w, h, rgba)| {
                ctx.load_texture(
                    format!("thumb-{}", g.slug()),
                    egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba),
                    TextureOptions::LINEAR,
                )
            });
            self.thumbs.insert(g, tex);
        }
        self.thumbs.get(&g).cloned().flatten()
    }

    // ------------------------------------------------------------------
    // Background jobs (import / scan)
    // ------------------------------------------------------------------

    fn start_import_disc(&mut self, path: std::path::PathBuf) {
        let mut lib = self.lib.clone();
        let progress = Arc::new(Mutex::new((String::from("Verifying disc"), 0.0f32)));
        let (tx, rx) = channel();
        let p2 = progress.clone();
        std::thread::spawn(move || {
            let r = lib.import_disc(&path, &mut |f| p2.lock().unwrap().1 = f);
            let _ = tx.send(match r {
                Ok(g) => {
                    let e = &lib.games[&g];
                    let msg = if e.verified {
                        format!("{} imported and verified.", e.ident.title)
                    } else {
                        format!("{} imported. This dump is not one Re:Compiled has verified, but it identifies as the right game.", e.ident.title)
                    };
                    JobResult::Library(lib, msg)
                }
                Err(e) => JobResult::Failed(format!("{e:#}")),
            });
        });
        self.job = Some(Job { label: "Importing disc".into(), progress, rx });
    }

    fn start_scan(&mut self, dir: std::path::PathBuf) {
        let mut lib = self.lib.clone();
        let progress = Arc::new(Mutex::new((String::from("Scanning"), 0.0f32)));
        let (tx, rx) = channel();
        let p2 = progress.clone();
        std::thread::spawn(move || {
            let report = lib.scan_folder(&dir, &mut |name, f| *p2.lock().unwrap() = (format!("Verifying {name}"), f));
            let mut msg = String::new();
            for (_, t, v) in &report.games {
                msg.push_str(&format!("{} {}\n", if *v { "✔" } else { "•" }, t));
            }
            for b in &report.bios {
                msg.push_str(&format!("✔ BIOS {b}\n"));
            }
            if msg.is_empty() {
                msg = "No supported discs or BIOS images were found in that folder.".into();
            }
            let _ = tx.send(JobResult::Library(lib, msg.trim_end().to_string()));
        });
        self.job = Some(Job { label: "Scanning folder".into(), progress, rx });
    }

    fn poll_job(&mut self) {
        let Some(job) = &self.job else { return };
        if let Ok(r) = job.rx.try_recv() {
            self.job = None;
            match r {
                JobResult::Library(lib, msg) => {
                    self.lib = lib;
                    if let Err(e) = self.lib.save(&self.paths) {
                        self.notify(format!("Could not save library: {e}"), true);
                    } else {
                        self.notify(msg, false);
                    }
                }
                JobResult::Failed(e) => self.notify(e, true),
            }
        }
    }

    // ------------------------------------------------------------------
    // Screens
    // ------------------------------------------------------------------

    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top")
            .frame(egui::Frame::default().fill(PANEL_BG).inner_margin(egui::Margin::symmetric(18, 12)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("DIGIMON WORLDS").size(22.0).strong().color(Color32::WHITE));
                    ui.label(RichText::new("Re:Compiled").size(22.0).strong().color(ACCENT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.selectable_label(self.screen == Screen::Settings, "⚙ Settings").clicked() {
                            self.screen = Screen::Settings;
                        }
                        if ui.selectable_label(self.screen == Screen::Library, "▶ Games").clicked() {
                            self.screen = Screen::Library;
                        }
                    });
                });
            });
    }

    fn library_screen(&mut self, ctx: &egui::Context) {
        self.top_bar(ctx);
        egui::TopBottomPanel::bottom("bios")
            .frame(egui::Frame::default().fill(PANEL_BG).inner_margin(egui::Margin::symmetric(18, 10)))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("BIOS").strong());
                    for region in [dwrc_disc::ident::Region::NtscU, dwrc_disc::ident::Region::Pal, dwrc_disc::ident::Region::NtscJ] {
                        let (txt, col) = match self.lib.bios_for(region) {
                            Some(b) => (
                                format!("✔ {} – {}", region_label(region), b.info.known_name.clone().unwrap_or_else(|| b.info.version.clone())),
                                OK,
                            ),
                            None => (format!("– {}", region_label(region)), MUTED),
                        };
                        ui.label(RichText::new(txt).color(col));
                        ui.add_space(10.0);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Add BIOS…").clicked() && self.job.is_none() {
                            if let Some(p) = rfd::FileDialog::new().set_title("Select your PlayStation BIOS dump").pick_file() {
                                match self.lib.import_bios(&p) {
                                    Ok(info) => {
                                        let _ = self.lib.save(&self.paths);
                                        let what = info.known_name.clone().unwrap_or(info.version.clone());
                                        let note = if info.known_name.is_some() { "verified" } else { "not a verified dump, but usable" };
                                        self.notify(format!("Added BIOS {what} ({}, {note}).", region_label(info.region)), false);
                                    }
                                    Err(e) => self.notify(e.to_string(), true),
                                }
                            }
                        }
                    });
                });
            });
        egui::CentralPanel::default().frame(egui::Frame::default().fill(Color32::from_rgb(13, 14, 18)).inner_margin(24.0)).show(ctx, |ui| {
            let none_imported = self.lib.games.is_empty();
            if none_imported {
                ui.vertical_centered(|ui| {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Bring your own discs").size(18.0).strong());
                    ui.label(
                        RichText::new(
                            "Re:Compiled runs your own disc images (.cue/.bin) and your console's BIOS. Nothing is copied or modified. \
                             Point it at the folder where you keep them:",
                        )
                        .color(MUTED),
                    );
                    ui.add_space(6.0);
                    if ui.add(egui::Button::new(RichText::new("Scan a folder for discs & BIOS…").size(16.0)).fill(ACCENT_DIM)).clicked()
                        && self.job.is_none()
                    {
                        if let Some(d) = rfd::FileDialog::new().set_title("Folder with your disc images and BIOS").pick_folder() {
                            self.start_scan(d);
                        }
                    }
                    ui.add_space(12.0);
                });
            }
            let avail = ui.available_width();
            let gap = 18.0;
            let card_w = ((avail - gap * 2.0) / 3.0).clamp(220.0, 420.0);
            let cols = if avail >= card_w * 3.0 + gap * 2.0 { 3 } else { 1 };
            egui::ScrollArea::vertical().show(ui, |ui| {
                if cols == 3 {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for g in GameId::ALL {
                            self.game_card(ui, ctx, g, card_w);
                        }
                    });
                } else {
                    for g in GameId::ALL {
                        self.game_card(ui, ctx, g, avail.min(520.0));
                        ui.add_space(gap);
                    }
                }
                if !none_imported {
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        if ui.button("Scan a folder…").clicked() && self.job.is_none() {
                            if let Some(d) = rfd::FileDialog::new().pick_folder() {
                                self.start_scan(d);
                            }
                        }
                        ui.label(RichText::new(format!("Data folder: {}", self.paths.root.display())).color(MUTED).small());
                    });
                }
            });
        });
    }

    fn game_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, g: GameId, w: f32) {
        let status = self.lib.status(g);
        let entry = self.lib.games.get(&g).cloned();
        let thumb = self.thumbnail(ctx, g);
        let enabled_mods = self.cfg.game_mods(g).enabled.len();
        egui::Frame::default().fill(CARD_BG).corner_radius(10.0).inner_margin(14.0).show(ui, |ui| {
          ui.vertical(|ui| {
            ui.set_width(w - 28.0);
            // Artwork: last-played screenshot or a placeholder.
            let art_h = (w - 28.0) * 0.75;
            let (rect, _) = ui.allocate_exact_size(Vec2::new(w - 28.0, art_h), Sense::hover());
            if let Some(t) = &thumb {
                ui.painter().image(t.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            } else {
                let p = ui.painter();
                p.rect_filled(rect, 6.0, Color32::from_rgb(22, 26, 34));
                let n = match g {
                    GameId::Dw1 => "1",
                    GameId::Dw2 => "2",
                    GameId::Dw3 => "3",
                };
                p.text(rect.center(), Align2::CENTER_CENTER, n, egui::FontId::proportional(art_h * 0.45), Color32::from_rgb(45, 52, 66));
                p.text(
                    rect.center_bottom() - Vec2::new(0.0, 16.0),
                    Align2::CENTER_CENTER,
                    "plays from your disc",
                    egui::FontId::proportional(13.0),
                    MUTED,
                );
            }
            ui.add_space(8.0);
            ui.label(RichText::new(g.title()).size(19.0).strong().color(Color32::WHITE));
            match &entry {
                Some(e) => {
                    ui.label(RichText::new(format!("{}  ·  {}", e.ident.title, e.ident.serial)).color(MUTED));
                }
                None => {
                    ui.label(RichText::new("Not imported yet").color(MUTED));
                }
            }
            let (chip, col) = match (&status, &entry) {
                (DiscStatus::Ready, Some(e)) if e.verified => ("✔ Verified dump", OK),
                (DiscStatus::Ready, Some(_)) => ("⚠ Unverified dump", WARN),
                (DiscStatus::Missing, _) => ("✖ Disc image missing", BAD),
                (DiscStatus::Changed, _) => ("✖ Disc image changed", BAD),
                _ => ("Import your disc to play", MUTED),
            };
            ui.label(RichText::new(chip).color(col));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let can_play = status == DiscStatus::Ready;
                let play = ui.add_enabled(
                    can_play,
                    egui::Button::new(RichText::new("▶  Play").size(16.0).strong().color(Color32::BLACK))
                        .fill(ACCENT)
                        .min_size(Vec2::new(100.0, 32.0)),
                );
                if play.clicked() {
                    self.start_game(g);
                }
                if ui.button(if entry.is_some() { "Re-import…" } else { "Import disc…" }).clicked() && self.job.is_none() {
                    if let Some(p) = rfd::FileDialog::new()
                        .set_title(format!("Select your {} disc image", g.title()))
                        .add_filter("Disc image", &["cue", "bin", "CUE", "BIN"])
                        .pick_file()
                    {
                        self.start_import_disc(p);
                    }
                }
                let label = if enabled_mods > 0 { format!("Mods ({enabled_mods})") } else { "Mods".to_string() };
                if ui.button(label).clicked() {
                    self.mods_cache.remove(&g);
                    self.selected_mod = None;
                    self.screen = Screen::Mods(g);
                }
            });
          });
        });
    }

    fn settings_screen(&mut self, ctx: &egui::Context) {
        self.top_bar(ctx);
        egui::CentralPanel::default().frame(egui::Frame::default().fill(Color32::from_rgb(13, 14, 18)).inner_margin(24.0)).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.set_max_width(720.0);
                let mut changed = false;
                section(ui, "Video", |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Scaling filter");
                        changed |= ui.radio_value(&mut self.cfg.video.filter, Filter::Nearest, "Sharp (original pixels)").changed();
                        changed |= ui.radio_value(&mut self.cfg.video.filter, Filter::Linear, "Smooth").changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label("Aspect ratio");
                        changed |= ui.radio_value(&mut self.cfg.video.aspect, Aspect::Tv4x3, "4:3 TV").changed();
                        changed |= ui.radio_value(&mut self.cfg.video.aspect, Aspect::IntegerScale, "Integer scale").changed();
                        changed |= ui.radio_value(&mut self.cfg.video.aspect, Aspect::Stretch, "Stretch").changed();
                    });
                    changed |= ui.checkbox(&mut self.cfg.video.show_fps, "Show frame rate").changed();
                    changed |= ui.checkbox(&mut self.cfg.video.start_fullscreen, "Start in fullscreen (F11 toggles)").changed();
                });
                section(ui, "Audio", |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Volume");
                        if ui.add(egui::Slider::new(&mut self.cfg.audio.volume, 0.0..=1.0).show_value(false)).changed() {
                            changed = true;
                        }
                        changed |= ui.checkbox(&mut self.cfg.audio.muted, "Mute").changed();
                    });
                    if let Some(e) = &self.audio_error {
                        ui.label(RichText::new(format!("Audio unavailable: {e}")).color(BAD));
                    }
                });
                section(ui, "Emulation", |ui| {
                    changed |= ui
                        .checkbox(&mut self.cfg.emulation.fast_boot, "Fast boot (skip the PlayStation logo; the BIOS still initialises normally)")
                        .changed();
                    changed |= ui.checkbox(&mut self.cfg.emulation.memory_card_2, "Insert a memory card in slot 2").changed();
                    ui.horizontal(|ui| {
                        ui.label("CPU engine");
                        changed |= ui.radio_value(&mut self.cfg.emulation.engine, EngineKind::Recompiled, "Recompiled (native)").changed();
                        changed |= ui.radio_value(&mut self.cfg.emulation.engine, EngineKind::Interpreter, "Reference interpreter").changed();
                        changed |= ui.radio_value(&mut self.cfg.emulation.engine, EngineKind::Validate, "Validate (developers)").changed();
                    });
                    ui.label(
                        RichText::new("Both engines run the same hardware model; the interpreter is the accuracy reference.").color(MUTED).small(),
                    );
                });
                section(ui, "Controls", |ui| {
                    ui.label(RichText::new("Click a binding, then press a key. Gamepads are detected automatically.").color(MUTED).small());
                    let pads = self.input.gamepad_names();
                    ui.label(if pads.is_empty() { "No gamepad connected".to_string() } else { format!("Gamepad: {}", pads.join(", ")) });
                    egui::Grid::new("keys").num_columns(4).spacing([16.0, 6.0]).show(ui, |ui| {
                        for (i, b) in input::EDITABLE.iter().enumerate() {
                            let name = input::button_name(*b);
                            let key = self.cfg.input.keyboard.get(&name).cloned().unwrap_or_else(|| "—".into());
                            ui.label(pretty_button(*b));
                            let txt = if self.rebinding == Some(*b) { "press a key…".to_string() } else { key };
                            if ui.add(egui::Button::new(txt).min_size(Vec2::new(110.0, 0.0))).clicked() {
                                self.rebinding = Some(*b);
                            }
                            if i % 2 == 1 {
                                ui.end_row();
                            }
                        }
                    });
                    if ui.button("Reset to defaults").clicked() {
                        self.cfg.input = Default::default();
                        changed = true;
                    }
                });
                section(ui, "Data", |ui| {
                    ui.label(format!("Saves, settings and generated data: {}", self.paths.root.display()));
                    ui.label(
                        RichText::new(if self.paths.portable {
                            "Portable mode is on (portable.txt next to the program)."
                        } else {
                            "Tip: put an empty file named portable.txt next to the program to keep everything beside it."
                        })
                        .color(MUTED)
                        .small(),
                    );
                });
                if let Some(b) = self.rebinding {
                    let pressed = ctx.input(|i| {
                        i.events.iter().find_map(|e| match e {
                            egui::Event::Key { key, pressed: true, .. } => Some(*key),
                            _ => None,
                        })
                    });
                    if let Some(k) = pressed {
                        if k != Key::Escape {
                            self.cfg.input.keyboard.insert(input::button_name(b), k.name().to_string());
                            changed = true;
                        }
                        self.rebinding = None;
                    }
                }
                if changed {
                    self.audio.set_volume(if self.cfg.audio.muted { 0.0 } else { self.cfg.audio.volume });
                    self.save_cfg();
                }
            });
        });
    }

    /// Shared by the Mods screen and the in-game (F10) overlay.
    fn mod_manager_ui(&mut self, ui: &mut egui::Ui, g: GameId) {
        let mods = self.mods_for(g).clone();
        let mut gm = self.cfg.game_mods(g);
        let mut changed = false;
        if mods.is_empty() {
            ui.label("No mods installed for this game yet.");
        }
        ui.horizontal(|ui| {
            if ui.button("Open mods folder").clicked() {
                let dir = self.paths.user_mods_root().join(g.slug());
                let _ = std::fs::create_dir_all(&dir);
                open_folder(&dir);
            }
            if ui.button("⟳ Rescan").clicked() {
                self.mods_cache.remove(&g);
            }
        });
        ui.label(
            RichText::new(format!(
                "Put mod folders in {} (each with a mod.toml). Enabled mods load top to bottom.",
                self.paths.user_mods_root().join(g.slug()).display()
            ))
            .color(MUTED)
            .small(),
        );
        ui.separator();
        // Enabled mods first in load order, then the rest.
        let mut order: Vec<&ModInfo> = gm.enabled.iter().filter_map(|id| mods.iter().find(|m| m.id() == id)).collect();
        for m in &mods {
            if !gm.enabled.iter().any(|e| e == m.id()) {
                order.push(m);
            }
        }
        for m in order {
            let id = m.id().to_string();
            let mut on = gm.enabled.contains(&id);
            ui.horizontal(|ui| {
                let resp = ui.add_enabled(m.is_usable(), egui::Checkbox::new(&mut on, ""));
                if resp.changed() {
                    if on {
                        gm.enabled.push(id.clone());
                    } else {
                        gm.enabled.retain(|e| e != &id);
                    }
                    changed = true;
                }
                let sel = self.selected_mod.as_deref() == Some(&id);
                if ui
                    .selectable_label(sel, RichText::new(format!("{}  v{}", m.manifest.info.name, m.manifest.info.version)).strong())
                    .clicked()
                {
                    self.selected_mod = if sel { None } else { Some(id.clone()) };
                }
                if !m.manifest.info.author.is_empty() {
                    ui.label(RichText::new(format!("by {}", m.manifest.info.author)).color(MUTED));
                }
                if on {
                    if let Some(pos) = gm.enabled.iter().position(|e| e == &id) {
                        if ui.small_button("Up").clicked() && pos > 0 {
                            gm.enabled.swap(pos, pos - 1);
                            changed = true;
                        }
                        if ui.small_button("Down").clicked() && pos + 1 < gm.enabled.len() {
                            gm.enabled.swap(pos, pos + 1);
                            changed = true;
                        }
                    }
                }
            });
            for p in &m.problems {
                ui.label(RichText::new(format!("   ✖ {p}")).color(BAD));
            }
            if self.selected_mod.as_deref() == Some(&id) {
                egui::Frame::default().fill(Color32::from_rgb(24, 27, 33)).inner_margin(10.0).corner_radius(6.0).show(ui, |ui| {
                    if !m.manifest.info.description.is_empty() {
                        ui.label(&m.manifest.info.description);
                    }
                    let saved = gm.options.entry(id.clone()).or_default();
                    for o in &m.manifest.options {
                        let cur = saved.get(&o.id).cloned().unwrap_or_else(|| o.default.clone());
                        ui.horizontal(|ui| {
                            ui.label(&o.name);
                            match o.kind {
                                OptionKind::Bool => {
                                    let mut v = cur.as_bool().unwrap_or(false);
                                    if ui.checkbox(&mut v, "").changed() {
                                        saved.insert(o.id.clone(), toml::Value::Boolean(v));
                                        changed = true;
                                    }
                                }
                                OptionKind::Int => {
                                    let mut v = cur.as_integer().unwrap_or(0);
                                    let lo = o.min.unwrap_or(i32::MIN as f64) as i64;
                                    let hi = o.max.unwrap_or(i32::MAX as f64) as i64;
                                    if ui.add(egui::DragValue::new(&mut v).range(lo..=hi)).changed() {
                                        saved.insert(o.id.clone(), toml::Value::Integer(v));
                                        changed = true;
                                    }
                                }
                                OptionKind::Float => {
                                    let mut v = cur.as_float().or_else(|| cur.as_integer().map(|i| i as f64)).unwrap_or(0.0);
                                    let lo = o.min.unwrap_or(f64::MIN);
                                    let hi = o.max.unwrap_or(f64::MAX);
                                    if ui.add(egui::DragValue::new(&mut v).range(lo..=hi).speed(0.05)).changed() {
                                        saved.insert(o.id.clone(), toml::Value::Float(v));
                                        changed = true;
                                    }
                                }
                                OptionKind::Choice => {
                                    let mut v = cur.as_str().unwrap_or("").to_string();
                                    egui::ComboBox::from_id_salt(format!("{id}-{}", o.id)).selected_text(&v).show_ui(ui, |ui| {
                                        for c in &o.choices {
                                            if ui.selectable_value(&mut v, c.clone(), c).changed() {
                                                saved.insert(o.id.clone(), toml::Value::String(v.clone()));
                                                changed = true;
                                            }
                                        }
                                    });
                                }
                            }
                            if !o.description.is_empty() {
                                ui.label(RichText::new(&o.description).color(MUTED).small());
                            }
                        });
                    }
                });
            }
        }
        if changed {
            *self.cfg.game_mods_mut(g) = gm;
            self.save_cfg();
            if self.emu.is_some() {
                self.restart_needed = true;
            }
        }
    }

    fn mods_screen(&mut self, ctx: &egui::Context, g: GameId) {
        self.top_bar(ctx);
        egui::CentralPanel::default().frame(egui::Frame::default().fill(Color32::from_rgb(13, 14, 18)).inner_margin(24.0)).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("← Back").clicked() {
                    self.screen = Screen::Library;
                }
                ui.label(RichText::new(format!("Mods for {}", g.title())).size(20.0).strong());
            });
            ui.add_space(8.0);
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.set_max_width(820.0);
                self.mod_manager_ui(ui, g);
            });
        });
    }

    fn playing_screen(&mut self, ctx: &egui::Context) {
        let Some(emu) = &self.emu else {
            self.screen = Screen::Library;
            return;
        };
        let status = emu.shared.status.lock().unwrap().clone();
        if let Some(err) = status.error.clone() {
            self.stop_game();
            self.screen = Screen::Library;
            self.notify(format!("Could not start the game: {err}"), true);
            return;
        }
        // Input
        let overlay_open = self.show_pause || self.show_mods_overlay;
        let pad = self.input.poll(ctx, &self.cfg.input.keyboard, !overlay_open);
        let emu = self.emu.as_ref().unwrap();
        emu.set_pad(0, if overlay_open { Default::default() } else { pad });
        let (esc, f10, f11, f12, tab) = ctx.input(|i| {
            (i.key_pressed(Key::Escape), i.key_pressed(Key::F10), i.key_pressed(Key::F11), i.key_pressed(Key::F12), i.key_down(Key::Tab))
        });
        if esc {
            if self.show_mods_overlay {
                self.show_mods_overlay = false;
            } else {
                self.show_pause = !self.show_pause;
            }
        }
        if f10 {
            self.show_mods_overlay = !self.show_mods_overlay;
        }
        if f11 {
            self.fullscreen = !self.fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
        }
        emu.shared.paused.store(overlay_open, Ordering::Relaxed);
        emu.shared.fast_forward.store(tab && !overlay_open, Ordering::Relaxed);
        // Frame upload
        {
            let f = emu.shared.frame.lock().unwrap();
            if f.seq != self.last_seq && f.width > 0 {
                self.last_seq = f.seq;
                let mut rgba = Vec::with_capacity(f.pixels.len() * 4);
                for p in &f.pixels {
                    rgba.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]);
                }
                let img = egui::ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &rgba);
                let opts = match self.cfg.video.filter {
                    Filter::Nearest => TextureOptions::NEAREST,
                    Filter::Linear => TextureOptions::LINEAR,
                };
                match &mut self.game_tex {
                    Some(t) => t.set(img, opts),
                    None => self.game_tex = Some(ctx.load_texture("game", img, opts)),
                }
                self.game_size = (f.width, f.height);
            }
            if f12 && f.width > 0 {
                let dir = self.paths.screenshots_dir();
                let _ = std::fs::create_dir_all(&dir);
                let name = format!(
                    "{}-{}.png",
                    emu.game.slug(),
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
                );
                let r = save_png(&dir.join(&name), f.width, f.height, &f.pixels);
                drop(f);
                match r {
                    Ok(_) => self.notify(format!("Screenshot saved: {name}"), false),
                    Err(e) => self.notify(format!("Screenshot failed: {e}"), true),
                }
            }
        }
        egui::CentralPanel::default().frame(egui::Frame::default().fill(Color32::BLACK)).show(ctx, |ui| {
            let avail = ui.max_rect();
            if let Some(t) = &self.game_tex {
                let (w, h) = (self.game_size.0 as f32, self.game_size.1 as f32);
                let size = match self.cfg.video.aspect {
                    Aspect::Stretch => avail.size(),
                    Aspect::Tv4x3 => {
                        let s = (avail.width() / 4.0).min(avail.height() / 3.0);
                        Vec2::new(s * 4.0, s * 3.0)
                    }
                    Aspect::IntegerScale => {
                        let k = (avail.width() / w).min(avail.height() / h).floor().max(1.0);
                        Vec2::new(w * k, h * k)
                    }
                };
                let rect = egui::Rect::from_center_size(avail.center(), size);
                ui.painter().image(t.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            } else {
                ui.painter().text(avail.center(), Align2::CENTER_CENTER, "Starting…", egui::FontId::proportional(20.0), MUTED);
            }
            if self.cfg.video.show_fps {
                ui.painter().text(
                    avail.left_top() + Vec2::new(8.0, 6.0),
                    Align2::LEFT_TOP,
                    format!("{:.1} fps · {:.0}% · {}", status.fps, status.speed * 100.0, status.engine),
                    egui::FontId::monospace(13.0),
                    Color32::from_rgb(200, 255, 200),
                );
            }
        });
        if self.show_pause {
            let mut quit = false;
            let mut reset = false;
            egui::Window::new("Paused")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_min_width(260.0);
                    ui.label(RichText::new(&status.title).strong());
                    ui.label(RichText::new(format!("Engine: {}", status.engine)).color(MUTED).small());
                    ui.add_space(6.0);
                    let b = |ui: &mut egui::Ui, t: &str| ui.add(egui::Button::new(t).min_size(Vec2::new(240.0, 30.0))).clicked();
                    if b(ui, "Resume (Esc)") {
                        self.show_pause = false;
                    }
                    if b(ui, "Mod manager (F10)") {
                        self.show_pause = false;
                        self.show_mods_overlay = true;
                    }
                    if b(ui, "Reset console") {
                        reset = true;
                    }
                    if b(ui, if self.fullscreen { "Leave fullscreen (F11)" } else { "Fullscreen (F11)" }) {
                        self.fullscreen = !self.fullscreen;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
                    }
                    ui.horizontal(|ui| {
                        ui.label("Volume");
                        if ui.add(egui::Slider::new(&mut self.cfg.audio.volume, 0.0..=1.0).show_value(false)).changed() {
                            self.audio.set_volume(if self.cfg.audio.muted { 0.0 } else { self.cfg.audio.volume });
                        }
                    });
                    ui.add_space(4.0);
                    if b(ui, "Quit to game list") {
                        quit = true;
                    }
                    ui.label(RichText::new("Memory cards save automatically.").color(MUTED).small());
                });
            if reset {
                if let Some(e) = &self.emu {
                    e.send(EmuCmd::Reset);
                }
                self.show_pause = false;
            }
            if quit {
                self.stop_game();
                self.save_cfg();
                self.screen = Screen::Library;
                return;
            }
        }
        if self.show_mods_overlay {
            let g = self.emu.as_ref().unwrap().game;
            let mut open = true;
            let mut restart = false;
            egui::Window::new(format!("Mods – {}", g.title()))
                .open(&mut open)
                .default_width(560.0)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    if !status.mod_log.is_empty() {
                        egui::CollapsingHeader::new("Mod log").default_open(false).show(ui, |ui| {
                            for l in &status.mod_log {
                                ui.label(RichText::new(l).monospace().small());
                            }
                        });
                    }
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        self.mod_manager_ui(ui, g);
                    });
                    if self.restart_needed {
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Changes apply when the game restarts.").color(WARN));
                            if ui.button("Restart now").clicked() {
                                restart = true;
                            }
                        });
                    }
                });
            if !open {
                self.show_mods_overlay = false;
            }
            if restart {
                self.start_game(g);
            }
        }
        ctx.request_repaint();
    }

    fn overlays(&mut self, ctx: &egui::Context) {
        if let Some(job) = &self.job {
            let (what, f) = job.progress.lock().unwrap().clone();
            egui::Window::new(&job.label)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_min_width(360.0);
                    ui.label(what);
                    ui.add(egui::ProgressBar::new(f).show_percentage());
                    ui.label(RichText::new("Checking the image against known-good dumps (SHA-1)…").color(MUTED).small());
                });
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if let Some((msg, t, err)) = &self.toast {
            let age = t.elapsed().as_secs_f32();
            let life = if *err { 9.0 } else { 6.0 };
            if age > life {
                self.toast = None;
            } else {
                egui::Area::new(egui::Id::new("toast")).anchor(Align2::CENTER_BOTTOM, [0.0, -60.0]).show(ctx, |ui| {
                    egui::Frame::default()
                        .fill(if *err { Color32::from_rgb(70, 24, 24) } else { Color32::from_rgb(24, 40, 30) })
                        .stroke(Stroke::new(1.0_f32, if *err { BAD } else { OK }))
                        .corner_radius(8.0)
                        .inner_margin(12.0)
                        .show(ui, |ui| {
                            ui.set_max_width(560.0);
                            ui.label(RichText::new(msg).color(Color32::WHITE));
                        });
                });
                ctx.request_repaint_after(Duration::from_millis(200));
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_job();
        if let Some(g) = self.autostart.take() {
            self.start_game(g);
        }
        let title = match (&self.screen, &self.emu) {
            (Screen::Playing, Some(e)) => format!("Digimon Worlds Re:Compiled – {}", e.game.title()),
            _ => "Digimon Worlds Re:Compiled".to_string(),
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        match self.screen {
            Screen::Library => self.library_screen(ctx),
            Screen::Settings => self.settings_screen(ctx),
            Screen::Mods(g) => self.mods_screen(ctx, g),
            Screen::Playing => self.playing_screen(ctx),
        }
        self.overlays(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop_game();
        self.save_cfg();
    }
}

fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::default().fill(CARD_BG).corner_radius(8.0).inner_margin(14.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).size(16.0).strong().color(BLUE));
        ui.add_space(4.0);
        add(ui);
    });
    ui.add_space(12.0);
}

fn pretty_button(b: Button) -> &'static str {
    match b {
        Button::Up => "D-pad Up",
        Button::Down => "D-pad Down",
        Button::Left => "D-pad Left",
        Button::Right => "D-pad Right",
        Button::Cross => "✕ Cross",
        Button::Circle => "○ Circle",
        Button::Square => "□ Square",
        Button::Triangle => "△ Triangle",
        Button::Start => "Start",
        Button::Select => "Select",
        Button::L1 => "L1",
        Button::R1 => "R1",
        Button::L2 => "L2",
        Button::R2 => "R2",
        Button::L3 => "L3",
        Button::R3 => "R3",
    }
}

fn style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL_BG;
    v.window_fill = Color32::from_rgb(24, 27, 33);
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    v.hyperlink_color = BLUE;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.button_padding = Vec2::new(10.0, 5.0);
        s.spacing.item_spacing = Vec2::new(8.0, 7.0);
    });
}

fn open_folder(p: &std::path::Path) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(p).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(p).spawn();
}

pub fn save_png(path: &std::path::Path, w: u32, h: u32, argb: &[u32]) -> anyhow::Result<()> {
    let f = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header()?;
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for p in argb {
        data.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
    }
    wr.write_image_data(&data)?;
    Ok(())
}

fn load_png(path: &std::path::Path) -> Option<(u32, u32, Vec<u8>)> {
    let f = std::fs::File::open(path).ok()?;
    let dec = png::Decoder::new(std::io::BufReader::new(f));
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    let rgba = match info.color_type {
        png::ColorType::Rgb => buf[..(w * h * 3) as usize].chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::Rgba => buf[..(w * h * 4) as usize].to_vec(),
        _ => return None,
    };
    Some((w, h, rgba))
}
