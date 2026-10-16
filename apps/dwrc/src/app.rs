//! The launcher / player user interface.

use crate::audio::{self, AudioOut, AudioShared};
use crate::emu::{EmuCmd, EmuHandle};
use crate::input::{self, Input};
use dwrc_disc::GameId;
use crate::mods_page::{ModsUi, Tab};
use dwrc_psx::Button;
use dwrc_runtime::bios::region_label;
use dwrc_runtime::config::{Aspect, BiosMode, EngineKind, Filter};
use dwrc_runtime::{Config, DiscStatus, Library, Paths};
use eframe::egui::{self, Align, Align2, Color32, Key, Layout, RichText, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) const ACCENT: Color32 = Color32::from_rgb(255, 140, 26);
pub(crate) const ACCENT_DIM: Color32 = Color32::from_rgb(120, 70, 20);
pub(crate) const BLUE: Color32 = Color32::from_rgb(64, 156, 255);
pub(crate) const CARD_BG: Color32 = Color32::from_rgb(28, 31, 38);
pub(crate) const PANEL_BG: Color32 = Color32::from_rgb(18, 20, 25);
pub(crate) const OK: Color32 = Color32::from_rgb(90, 200, 120);
pub(crate) const WARN: Color32 = Color32::from_rgb(240, 190, 70);
pub(crate) const BAD: Color32 = Color32::from_rgb(235, 90, 90);
pub(crate) const MUTED: Color32 = Color32::from_rgb(150, 156, 168);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Library,
    Mods,
    Settings,
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
    mods_ui: ModsUi,
    autostart: Option<GameId>,
    files_hovering: bool,
    open_files: Vec<std::path::PathBuf>,
    updater: crate::footer::Updater,
    /// The CRT filter (None when OpenGL 3 shaders are unavailable).
    crt: Option<Arc<crate::crt::Crt>>,
    crt_error: Option<String>,
    /// Height of the Games page content last frame (to centre it).
    library_h: f32,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, autostart: Option<GameId>, open_files: Vec<std::path::PathBuf>) -> Self {
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
        let mods_ui = ModsUi::new(&paths);
        let updater = crate::footer::Updater::new(&paths, cfg.updates.check_on_start);
        let (crt, crt_error) = match cc.gl.as_ref().map(|gl| crate::crt::Crt::new(gl)) {
            Some(Ok(c)) => (Some(Arc::new(c)), None),
            Some(Err(e)) => {
                log::warn!("CRT filter unavailable: {e}");
                (None, Some(e))
            }
            None => (None, Some("needs the OpenGL renderer".to_string())),
        };
        Self {
            mods_ui,
            updater,
            crt,
            crt_error,
            library_h: 0.0,
            files_hovering: false,
            open_files,
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
            autostart,
        }
    }

    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.fullscreen = !self.fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
    }

    fn notify(&mut self, msg: impl Into<String>, error: bool) {
        self.toast = Some((msg.into(), Instant::now(), error));
    }

    fn save_cfg(&mut self) {
        if let Err(e) = self.cfg.save(&self.paths) {
            self.notify(format!("Could not save settings: {e}"), true);
        }
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
        self.mods_ui.changed_while_running = false;
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
                        let close = egui::Button::new(RichText::new("Close").size(16.0).strong().color(MUTED))
                            .fill(Color32::TRANSPARENT)
                            .min_size(Vec2::new(96.0, 30.0));
                        if ui.add(close).on_hover_text("Quit Digimon Worlds Re:Compiled").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        for (screen, label) in [(Screen::Settings, "⚙ Settings"), (Screen::Mods, "Mods"), (Screen::Library, "▶ Games")] {
                            let sel = self.screen == screen;
                            let txt = RichText::new(label).size(16.0).strong().color(if sel { Color32::WHITE } else { MUTED });
                            let b = egui::Button::new(txt).fill(if sel { ACCENT_DIM } else { Color32::TRANSPARENT }).min_size(Vec2::new(96.0, 30.0));
                            if ui.add(b).clicked() {
                                self.screen = screen;
                            }
                        }
                    });
                });
            });
    }

    fn library_screen(&mut self, ctx: &egui::Context) {
        self.top_bar(ctx);
        if let Some(m) = crate::footer::footer(ctx, &mut self.updater) {
            self.notify(m, true);
        }
        egui::CentralPanel::default().frame(egui::Frame::default().fill(Color32::from_rgb(13, 14, 18)).inner_margin(24.0)).show(ctx, |ui| {
            let none_imported = self.lib.games.is_empty();
            let page = ui.available_rect_before_wrap();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                // Centre everything vertically while it fits (its height is
                // measured each frame and used on the next).
                ui.add_space(((page.height() - self.library_h) / 2.0).max(0.0));
                let top = ui.cursor().top();
                if none_imported {
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("Bring your own discs").size(20.0).strong().color(Color32::WHITE));
                        ui.label(
                            RichText::new(
                                "Re:Compiled runs your own disc images: the .bin is all it needs (a .cue is optional). \
                                 A BIOS dump is optional too (add one under Settings, BIOS). Nothing is copied or modified. Point it at the folder where you keep them:",
                            )
                            .color(MUTED),
                        );
                        ui.add_space(6.0);
                        if ui.add(egui::Button::new(RichText::new("Scan a folder for discs…").size(16.0).color(Color32::WHITE)).fill(ACCENT_DIM)).clicked()
                            && self.job.is_none()
                        {
                            if let Some(d) = rfd::FileDialog::new().set_title("Folder with your disc images and BIOS").pick_folder() {
                                self.start_scan(d);
                            }
                        }
                        ui.add_space(18.0);
                    });
                }
                // Cards: three across, as large as the page allows (by width
                // and by height), centred; one column on narrow windows.
                let gap = 24.0;
                let avail = ui.available_width();
                let used_above = ui.cursor().top() - top;
                let below = if none_imported { 0.0 } else { 70.0 };
                let height_budget = page.height() - used_above - below;
                let card_for = |w: f32| -> f32 {
                    let k = (w / 400.0).clamp(1.0, 1.3);
                    28.0 + (w - 28.0) * 0.75 + 150.0 * k
                };
                let mut card_w = ((avail - gap * 2.0) / 3.0).min(680.0);
                while card_w > 260.0 && card_for(card_w) > height_budget {
                    card_w -= 10.0;
                }
                if card_w >= 240.0 {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        ui.add_space(((avail - card_w * 3.0 - gap * 2.0) / 2.0).max(0.0));
                        for g in GameId::ALL {
                            self.game_card(ui, ctx, g, card_w);
                        }
                    });
                } else {
                    let w = avail.min(480.0);
                    for g in GameId::ALL {
                        ui.horizontal_top(|ui| {
                            ui.add_space(((avail - w) / 2.0).max(0.0));
                            self.game_card(ui, ctx, g, w);
                        });
                        ui.add_space(gap);
                    }
                }
                if !none_imported {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        if ui.button("Scan a folder for discs…").clicked() && self.job.is_none() {
                            if let Some(d) = rfd::FileDialog::new().pick_folder() {
                                self.start_scan(d);
                            }
                        }
                        ui.label(RichText::new(format!("Data folder: {}", self.paths.root.display())).color(MUTED).small());
                    });
                }
                let h = ui.cursor().top() - top;
                if (h - self.library_h).abs() > 0.5 {
                    self.library_h = h;
                    ctx.request_repaint();
                }
            });
        });
    }

    fn game_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, g: GameId, w: f32) {
        let status = self.lib.status(g);
        let entry = self.lib.games.get(&g).cloned();
        let thumb = self.thumbnail(ctx, g);
        let enabled_mods = {
            let env = crate::mods_page::Env { cfg: &mut self.cfg, paths: &self.paths, lib: &self.lib, running: None };
            self.mods_ui.active_count(&env, g)
        };
        // Text and buttons grow a little with the card.
        let k = (w / 400.0).clamp(1.0, 1.3);
        egui::Frame::default().fill(CARD_BG).corner_radius(10.0).inner_margin(14.0).show(ui, |ui| {
          ui.vertical(|ui| {
            ui.set_width(w - 28.0);
            ui.spacing_mut().item_spacing = Vec2::new(8.0 * k, 7.0);
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
                    egui::FontId::proportional(13.0 * k),
                    MUTED,
                );
            }
            ui.add_space(8.0);
            ui.label(RichText::new(g.title()).size(20.0 * k).strong().color(Color32::WHITE));
            match &entry {
                Some(e) => {
                    ui.label(RichText::new(format!("{}  ·  {}", e.ident.title, e.ident.serial)).size(13.5 * k).color(MUTED));
                }
                None => {
                    ui.label(RichText::new("Not imported yet").size(13.5 * k).color(MUTED));
                }
            }
            let (chip, col) = match (&status, &entry) {
                (DiscStatus::Ready, Some(e)) if e.verified => ("✔ Verified dump", OK),
                (DiscStatus::Ready, Some(_)) => ("⚠ Unverified dump", WARN),
                (DiscStatus::Missing, _) => ("✖ Disc image missing", BAD),
                (DiscStatus::Changed, _) => ("✖ Disc image changed", BAD),
                _ => ("Import your disc to play", MUTED),
            };
            ui.label(RichText::new(chip).size(13.5 * k).color(col));
            ui.add_space(6.0 * k);
            ui.horizontal(|ui| {
                let can_play = status == DiscStatus::Ready;
                let play = ui.add_enabled(
                    can_play,
                    egui::Button::new(RichText::new("▶  Play").size(16.0 * k).strong().color(Color32::BLACK))
                        .fill(ACCENT)
                        .min_size(Vec2::new(100.0 * k, 32.0 * k)),
                );
                let small = |t: String| egui::Button::new(RichText::new(t).size(14.0 * k)).min_size(Vec2::new(0.0, 32.0 * k));
                if play.clicked() {
                    self.start_game(g);
                }
                if ui.add(small(if entry.is_some() { "Re-import…" } else { "Import disc…" }.to_string())).clicked() && self.job.is_none() {
                    if let Some(p) = rfd::FileDialog::new()
                        .set_title(format!("Select your {} disc image", g.title()))
                        .add_filter("Disc image", &["cue", "bin", "CUE", "BIN"])
                        .pick_file()
                    {
                        self.start_import_disc(p);
                    }
                }
                let label = if enabled_mods > 0 { format!("Mods ({enabled_mods})") } else { "Mods".to_string() };
                if ui.add(small(label)).on_hover_text(format!("Mods that load for {}", g.title())).clicked() {
                    self.mods_ui.game_filter = Some(g);
                    self.mods_ui.search.clear();
                    self.mods_ui.tab = Tab::Installed;
                    self.screen = Screen::Mods;
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
                        let crt_ok = self.crt.is_some();
                        let r = ui.add_enabled(crt_ok, egui::RadioButton::new(self.cfg.video.filter == Filter::Crt, "CRT"));
                        let r = match &self.crt_error {
                            Some(e) => r.on_disabled_hover_text(format!("The CRT filter is not available on this computer: {e}")),
                            None => r.on_hover_text("Scanlines and phosphors, like a CRT television"),
                        };
                        if r.clicked() {
                            self.cfg.video.filter = Filter::Crt;
                            changed = true;
                        }
                    });
                    if self.cfg.video.filter == Filter::Crt {
                        let crt = &mut self.cfg.video.crt;
                        egui::Grid::new("crt").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                            for (label, value, tip) in [
                                ("Scanlines", &mut crt.scanlines, "How dark the gaps between the picture's lines are"),
                                ("Phosphor mask", &mut crt.mask, "How visible the red, green and blue stripes are"),
                                ("Screen curvature", &mut crt.curvature, "0 is a flat screen"),
                            ] {
                                ui.label(label).on_hover_text(tip);
                                changed |= ui.add(egui::Slider::new(value, 0.0..=1.0).show_value(false)).changed();
                                ui.end_row();
                            }
                        });
                        if ui.button("Reset CRT settings").clicked() {
                            self.cfg.video.crt = Default::default();
                            changed = true;
                        }
                        if self.crt.is_none() {
                            ui.label(RichText::new("The CRT filter is not available on this computer, so Smooth is used.").color(WARN).small());
                        }
                    }
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
                let mut add_bios = false;
                let mut remove_bios: Option<usize> = None;
                section(ui, "BIOS", |ui| {
                    ui.label(
                        RichText::new(
                            "A BIOS dumped from your own console is optional. Without one, games start with the built-in OpenBIOS \
                             (open source, MIT). Game code runs the same with either; your own BIOS also makes the BIOS's own routines \
                             exact and allows fast boot. Saves work with both.",
                        )
                        .color(MUTED),
                    );
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Use");
                        changed |= ui
                            .radio_value(&mut self.cfg.emulation.bios, BiosMode::Auto, "Your console's BIOS when added, otherwise built-in")
                            .changed();
                        changed |= ui.radio_value(&mut self.cfg.emulation.bios, BiosMode::BuiltIn, "Always the built-in BIOS").changed();
                    });
                    ui.add_space(4.0);
                    egui::Grid::new("bios-regions").num_columns(2).spacing([18.0, 4.0]).show(ui, |ui| {
                        for region in [dwrc_disc::ident::Region::NtscU, dwrc_disc::ident::Region::Pal, dwrc_disc::ident::Region::NtscJ] {
                            ui.label(region_label(region));
                            match self.lib.bios_choice(region, self.cfg.emulation.bios) {
                                dwrc_runtime::library::BiosChoice::Console(b) => {
                                    let name = b.info.known_name.clone().unwrap_or_else(|| b.info.version.clone());
                                    ui.label(RichText::new(format!("✔ {name}")).color(OK));
                                }
                                dwrc_runtime::library::BiosChoice::BuiltIn => {
                                    ui.label(RichText::new("built-in OpenBIOS").color(MUTED));
                                }
                            }
                            ui.end_row();
                        }
                    });
                    if !self.lib.bios.is_empty() {
                        ui.add_space(6.0);
                        ui.label(RichText::new("Your BIOS files").strong());
                        for (i, b) in self.lib.bios.iter().enumerate() {
                            ui.horizontal_wrapped(|ui| {
                                let name = b.info.known_name.clone().unwrap_or_else(|| b.info.version.clone());
                                ui.label(RichText::new(name).strong());
                                ui.label(RichText::new(region_label(b.info.region)).color(MUTED));
                                if b.info.known_name.is_some() {
                                    ui.label(RichText::new("✔ verified dump").color(OK));
                                } else {
                                    ui.label(RichText::new("unverified dump").color(WARN));
                                }
                                if !b.path.exists() {
                                    ui.label(RichText::new("✖ file missing").color(BAD));
                                }
                                if ui.button("Remove").on_hover_text("Stop using this BIOS. The file itself is not deleted.").clicked() {
                                    remove_bios = Some(i);
                                }
                            });
                            ui.label(RichText::new(b.path.display().to_string()).color(MUTED).small());
                        }
                    }
                    ui.add_space(4.0);
                    if ui.button("Add BIOS…").clicked() {
                        add_bios = true;
                    }
                    changed |= ui
                        .checkbox(
                            &mut self.cfg.emulation.fast_boot,
                            "Fast boot (skip the PlayStation logo; with your own BIOS only, the built-in one boots straight into the game)",
                        )
                        .changed();
                });
                if add_bios && self.job.is_none() {
                    if let Some(p) = rfd::FileDialog::new().set_title("Select your PlayStation BIOS dump").pick_file() {
                        self.add_bios(p);
                    }
                }
                if let Some(i) = remove_bios {
                    let b = self.lib.bios.remove(i);
                    let _ = self.lib.save(&self.paths);
                    let name = b.info.known_name.unwrap_or(b.info.version);
                    self.notify(format!("Removed BIOS {name} from the list (the file was not deleted)."), false);
                }
                section(ui, "Emulation", |ui| {
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
                section(ui, "About", |ui| {
                    changed |= self.updater.settings(ui, &mut self.cfg.updates.check_on_start);
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

    fn mods_screen(&mut self, ctx: &egui::Context) {
        self.top_bar(ctx);
        egui::CentralPanel::default().frame(egui::Frame::default().fill(Color32::from_rgb(13, 14, 18)).inner_margin(24.0)).show(ctx, |ui| {
            let mut env = crate::mods_page::Env { cfg: &mut self.cfg, paths: &self.paths, lib: &self.lib, running: self.emu.as_ref().map(|e| e.game) };
            if self.mods_ui.show(ui, &mut env) {
                self.save_cfg();
            }
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
        let (esc, f10, f12, tab) =
            ctx.input(|i| (i.key_pressed(Key::Escape), i.key_pressed(Key::F10), i.key_pressed(Key::F12), i.key_down(Key::Tab)));
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
                if let (Filter::Crt, Some(crt)) = (self.cfg.video.filter, &self.crt) {
                    crt.set_frame(f.seq, f.width, f.height, &rgba);
                }
                let img = egui::ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &rgba);
                let opts = match self.cfg.video.filter {
                    Filter::Nearest => TextureOptions::NEAREST,
                    Filter::Linear | Filter::Crt => TextureOptions::LINEAR,
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
                match (self.cfg.video.filter, &self.crt) {
                    (Filter::Crt, Some(crt)) => {
                        let crt = crt.clone();
                        let c = self.cfg.video.crt;
                        let params = crate::crt::Params { scanlines: c.scanlines, mask: c.mask, curvature: c.curvature };
                        let cb = eframe::egui_glow::CallbackFn::new(move |_info, painter| crt.paint(painter.gl(), params));
                        ui.painter().add(egui::PaintCallback { rect, callback: Arc::new(cb) });
                    }
                    _ => {
                        ui.painter().image(t.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                }
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
                    ui.label(RichText::new(format!("Engine: {} · BIOS: {}", status.engine, status.bios)).color(MUTED).small());
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
                        self.toggle_fullscreen(ctx);
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
                    let mut changed = false;
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        let mut env = crate::mods_page::Env { cfg: &mut self.cfg, paths: &self.paths, lib: &self.lib, running: Some(g) };
                        changed = self.mods_ui.game_panel(ui, &mut env, g);
                    });
                    if changed {
                        self.save_cfg();
                    }
                    ui.label(RichText::new("Install, update and reorder mods on the Mods page (press Esc, then Quit to game list).").color(MUTED).small());
                    if self.mods_ui.changed_while_running {
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

    /// Files dropped on the window: a mod `.zip` is installed, a disc image
    /// is imported. (Not while playing, so a stray drop never interrupts.)
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let (hovering, dropped) = ctx.input(|i| (!i.raw.hovered_files.is_empty(), i.raw.dropped_files.clone()));
        self.files_hovering = hovering && self.screen != Screen::Playing;
        if self.screen == Screen::Playing {
            return;
        }
        let mut files: Vec<std::path::PathBuf> = std::mem::take(&mut self.open_files);
        files.extend(dropped.into_iter().filter_map(|f| f.path));
        for path in files {
            self.open_file(ctx, path);
        }
    }

    fn add_bios(&mut self, path: std::path::PathBuf) {
        match self.lib.import_bios(&path) {
            Ok(info) => {
                let _ = self.lib.save(&self.paths);
                let what = info.known_name.clone().unwrap_or(info.version.clone());
                let note = if info.known_name.is_some() { "verified" } else { "not a verified dump, but usable" };
                self.notify(format!("Added BIOS {what} ({}, {note}).", region_label(info.region)), false);
            }
            Err(e) => self.notify(e.to_string(), true),
        }
    }

    fn open_file(&mut self, ctx: &egui::Context, path: std::path::PathBuf) {
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let bios_sized = std::fs::metadata(&path).map(|m| m.len() == dwrc_runtime::bios::BIOS_SIZE as u64).unwrap_or(false);
        if bios_sized && matches!(ext.as_str(), "bin" | "rom" | "") {
            self.screen = Screen::Settings;
            self.add_bios(path);
            return;
        }
        match ext.as_str() {
            "zip" => {
                self.screen = Screen::Mods;
                let env = crate::mods_page::Env { cfg: &mut self.cfg, paths: &self.paths, lib: &self.lib, running: self.emu.as_ref().map(|e| e.game) };
                self.mods_ui.import_zip(ctx, &env, path);
            }
            "cue" | "bin" if self.job.is_none() => {
                self.screen = Screen::Library;
                self.start_import_disc(path);
            }
            _ => self.notify(format!("{} is not a mod (.zip), a disc image (.bin/.cue) or a BIOS dump.", path.display()), true),
        }
    }

    fn overlays(&mut self, ctx: &egui::Context) {
        if self.files_hovering {
            let screen = ctx.screen_rect();
            let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop")));
            p.rect_filled(screen, 0.0, Color32::from_black_alpha(190));
            p.rect_stroke(screen.shrink(18.0), 12.0, Stroke::new(3.0_f32, ACCENT), egui::StrokeKind::Inside);
            p.text(screen.center(), Align2::CENTER_CENTER, "Drop a mod .zip to install it,\na disc image (.bin/.cue) to import it,\nor a BIOS dump to add it", egui::FontId::proportional(24.0), Color32::WHITE);
            ctx.request_repaint();
        }
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
        self.updater.poll(ctx);
        let mut env = crate::mods_page::Env { cfg: &mut self.cfg, paths: &self.paths, lib: &self.lib, running: self.emu.as_ref().map(|e| e.game) };
        if self.mods_ui.poll(ctx, &mut env) {
            self.save_cfg();
        }
        for (msg, err) in std::mem::take(&mut self.mods_ui.toasts) {
            self.notify(msg, err);
        }
        self.handle_dropped_files(ctx);
        // F11 toggles fullscreen everywhere (but not while a key is being
        // bound in Settings). The window's real state wins, in case the
        // system changed it.
        let (f11, real_fs) = ctx.input(|i| (i.key_pressed(Key::F11), i.viewport().fullscreen));
        if let Some(fs) = real_fs {
            self.fullscreen = fs;
        }
        if f11 && self.rebinding.is_none() {
            self.toggle_fullscreen(ctx);
        }
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
            Screen::Mods => self.mods_screen(ctx),
            Screen::Playing => self.playing_screen(ctx),
        }
        if self.screen != Screen::Playing {
            if let Some(m) = self.updater.window(ctx) {
                self.notify(m, true);
            }
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
        Button::Cross => "Cross",
        Button::Circle => "Circle",
        Button::Square => "Square",
        Button::Triangle => "Triangle",
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
    // Always dark. egui otherwise follows the system theme, and with a light
    // Windows/macOS theme it drew dark text and light buttons on our dark
    // panels.
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL_BG;
    v.window_fill = Color32::from_rgb(24, 27, 33);
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    v.hyperlink_color = BLUE;
    v.extreme_bg_color = Color32::from_rgb(30, 34, 42);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    ctx.set_visuals_of(egui::Theme::Light, v.clone());
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.all_styles_mut(|s| {
        s.spacing.button_padding = Vec2::new(10.0, 5.0);
        s.spacing.item_spacing = Vec2::new(8.0, 7.0);
    });
}

pub(crate) fn open_folder(p: &std::path::Path) {
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
