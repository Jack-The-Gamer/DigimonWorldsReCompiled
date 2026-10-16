//! The bar along the bottom of the Games page: the running version, an
//! **Update** button that appears by itself when a newer Re:Compiled is
//! released, and links to the community Discord and to Ko-fi.
//!
//! The update itself (check, download, swapping the program's files) is in
//! `dwrc_modhub::app_update`; this is its user interface.

use crate::app::{ACCENT, ACCENT_DIM, BAD, MUTED, OK, PANEL_BG};
use dwrc_modhub::app_update::{self, AppCheck, AppRelease, Install, RUNNING};
use dwrc_modhub::{text, Hub, Progress};
use dwrc_runtime::Paths;
use eframe::egui::{self, epaint, Align, Align2, Color32, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

pub const DISCORD_URL: &str = "https://discord.gg/zyUF5mUt84";
pub const KOFI_URL: &str = "https://ko-fi.com/jackthegamer7";
pub const GITHUB_URL: &str = "https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled";
const DISCORD_FILL: Color32 = Color32::from_rgb(88, 101, 242);
const KOFI_FILL: Color32 = Color32::from_rgb(255, 94, 91);
const UPDATE_FILL: Color32 = Color32::from_rgb(214, 110, 10);

enum Task {
    Idle,
    Downloading { progress: Arc<Progress>, rx: Receiver<Result<PathBuf, String>> },
    /// Installed; the new program starts on the next launch.
    Ready { exe: PathBuf, version: String },
    Failed(String),
}

pub struct Updater {
    hub: Hub,
    downloads: PathBuf,
    install: Option<Install>,
    checking: Option<Receiver<AppCheck>>,
    check: Option<AppCheck>,
    window: bool,
    task: Task,
}

impl Updater {
    /// Starts the check when `check_now` (the player's setting).
    pub fn new(paths: &Paths, check_now: bool) -> Self {
        let install = Install::detect();
        if let Some(Install::Release { dir }) = &install {
            // Remove what the last update moved aside. Right after an update
            // the old program may still be closing, so try for a while.
            let dir = dir.clone();
            std::thread::spawn(move || {
                for _ in 0..15 {
                    if app_update::cleanup(&dir) == 0 {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(2));
                }
            });
        }
        let mut u = Self {
            hub: Hub::new(paths.modhub_cache_dir()),
            downloads: paths.downloads_dir(),
            install,
            checking: None,
            check: None,
            window: false,
            task: Task::Idle,
        };
        if check_now {
            u.check(false);
        }
        u
    }

    /// Asks GitHub (or the cache, unless `force`) for the latest release.
    pub fn check(&mut self, force: bool) {
        if self.checking.is_some() {
            return;
        }
        let hub = self.hub.clone();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(hub.check_app(force));
        });
        self.checking = Some(rx);
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.checking {
            match rx.try_recv() {
                Ok(c) => {
                    if let Some(e) = &c.error {
                        log::info!("update check: {e}");
                    }
                    self.check = Some(c);
                    self.checking = None;
                }
                Err(TryRecvError::Disconnected) => self.checking = None,
                Err(TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(250)),
            }
        }
        if let Task::Downloading { rx, .. } = &self.task {
            match rx.try_recv() {
                Ok(Ok(exe)) => {
                    let version = self.available().map(|r| r.version.clone()).unwrap_or_default();
                    self.task = Task::Ready { exe, version };
                }
                Ok(Err(e)) => self.task = Task::Failed(e),
                Err(TryRecvError::Disconnected) => self.task = Task::Failed("the update stopped unexpectedly".into()),
                Err(TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(100)),
            }
        }
    }

    /// The newer release, if there is one.
    pub fn available(&self) -> Option<&AppRelease> {
        self.check.as_ref().and_then(AppCheck::update)
    }

    fn start_update(&mut self) {
        let (Some(rel), Some(Install::Release { dir })) = (self.available().cloned(), self.install.clone()) else { return };
        let Some(zip) = rel.zip else { return };
        let progress = Arc::new(Progress::default());
        let (tx, rx) = channel();
        let (hub, p2, downloads) = (self.hub.clone(), progress.clone(), self.downloads.clone());
        std::thread::spawn(move || {
            let r = (|| -> anyhow::Result<PathBuf> {
                let file = hub.download_zip(&zip.url, &downloads, &p2)?;
                let r = app_update::apply(&file, &dir);
                let _ = std::fs::remove_file(&file);
                r
            })();
            let _ = tx.send(r.map_err(|e| format!("{e:#}")));
        });
        self.task = Task::Downloading { progress, rx };
    }

    /// Starts the updated program and closes this one. Returns a message
    /// when the new program could not be started.
    fn restart(&mut self, ctx: &egui::Context) -> Option<String> {
        let Task::Ready { exe, .. } = &self.task else { return None };
        match app_update::relaunch(exe) {
            Ok(()) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                None
            }
            Err(e) => Some(format!("Updated, but the new version could not be started ({e}). Start it yourself from {}.", exe.display())),
        }
    }

    /// The left part of the Games page footer. Returns a message to show.
    fn footer_left(&mut self, ui: &mut egui::Ui) -> Option<String> {
        ui.label(RichText::new(format!("Re:Compiled v{RUNNING}")).color(MUTED));
        let mut msg = None;
        match &self.task {
            Task::Downloading { progress, .. } => {
                let f = progress.fraction().unwrap_or(0.0);
                let v = self.available().map(|r| r.version.clone()).unwrap_or_default();
                let bar = egui::ProgressBar::new(f).desired_width(190.0).fill(ACCENT_DIM).text(RichText::new(format!("Updating to v{v}… {:.0}%", f * 100.0)).color(Color32::WHITE));
                let r = ui.add(bar);
                if r.interact(Sense::click()).clicked() {
                    self.window = true;
                }
            }
            Task::Ready { version, .. } => {
                let label = format!("Restart to use v{version}");
                if icon_button(ui, Icon::Restart, &label, UPDATE_FILL).on_hover_text("The update is installed").clicked() {
                    msg = self.restart(ui.ctx());
                }
            }
            Task::Idle | Task::Failed(_) => {
                if let Some(rel) = self.available() {
                    let label = format!("Update to v{}", rel.version);
                    let tip = if matches!(self.task, Task::Failed(_)) { "The last attempt failed. Click for details." } else { "A new version of Re:Compiled is out. Click to see what's new." };
                    if icon_button(ui, Icon::Download, &label, UPDATE_FILL).on_hover_text(tip).clicked() {
                        self.window = true;
                    }
                }
            }
        }
        msg
    }

    /// The update window (drawn on every page but the game).
    pub fn window(&mut self, ctx: &egui::Context) -> Option<String> {
        if !self.window {
            return None;
        }
        let Some(rel) = self.available().cloned() else {
            self.window = false;
            return None;
        };
        let mut open = true;
        let mut msg = None;
        egui::Window::new("Update Re:Compiled")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(540.0)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                let heading = if matches!(self.task, Task::Ready { .. }) { format!("Version {} is installed", rel.version) } else { format!("Version {} is available", rel.version) };
                ui.label(RichText::new(heading).size(18.0).strong());
                let when = rel.published.as_deref().map(|d| format!("Released {d}. ")).unwrap_or_default();
                ui.label(RichText::new(format!("{when}You have {RUNNING}.")).color(MUTED));
                if !rel.notes.trim().is_empty() {
                    ui.add_space(4.0);
                    egui::Frame::default().fill(Color32::from_rgb(20, 22, 28)).corner_radius(6.0).inner_margin(10.0).show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(240.0).auto_shrink([false, true]).show(ui, |ui| {
                            ui.label(text::clean_markdown(&rel.notes, 6000));
                        });
                    });
                }
                ui.hyperlink_to("Release page on GitHub", &rel.page);
                ui.separator();
                match (&self.install, &rel.zip) {
                    (Some(Install::Source { repo }), _) => {
                        ui.label(format!(
                            "This copy was built from the source code in {}. To update it, pull the latest changes \
                             (GitHub Desktop: Fetch origin, then Pull) and start it with the Play script again, which rebuilds it.",
                            repo.display()
                        ));
                    }
                    (Some(Install::Release { dir }), Some(zip)) => {
                        let mut start = false;
                        let mut restart = false;
                        let mut later = false;
                        match &self.task {
                            Task::Idle => {
                                let size = zip.size.map(|s| format!(" ({:.0} MB)", s as f64 / 1048576.0)).unwrap_or_default();
                                ui.label(format!(
                                    "Downloads the new version{size} and replaces the program in {}. Your games, saves, settings and mods are kept.",
                                    dir.display()
                                ));
                                ui.add_space(4.0);
                                start = ui.add(egui::Button::new(RichText::new("Update now").strong()).fill(ACCENT_DIM)).clicked();
                            }
                            Task::Downloading { progress, .. } => {
                                let f = progress.fraction().unwrap_or(0.0);
                                ui.add(egui::ProgressBar::new(f).show_percentage());
                                if ui.button("Cancel").clicked() {
                                    progress.cancel.store(true, Ordering::Relaxed);
                                }
                            }
                            Task::Ready { version, .. } => {
                                ui.label(RichText::new(format!("Updated to v{version}. Restart to start using it.")).color(OK));
                                ui.horizontal(|ui| {
                                    restart = ui.add(egui::Button::new(RichText::new("Restart now").strong()).fill(ACCENT_DIM)).clicked();
                                    later = ui.button("Later").clicked();
                                });
                            }
                            Task::Failed(e) => {
                                ui.label(RichText::new(format!("The update did not finish: {e}")).color(BAD));
                                start = ui.button("Try again").clicked();
                            }
                        }
                        if start {
                            self.start_update();
                        }
                        if restart {
                            msg = self.restart(ctx);
                        }
                        if later {
                            self.window = false;
                        }
                    }
                    _ => {
                        ui.label("Download it from the release page: there is no automatic update for this system.");
                    }
                }
            });
        if !open {
            self.window = false;
        }
        msg
    }

    /// Settings → About: the check setting and a "Check now" button.
    /// Returns whether a setting changed.
    pub fn settings(&mut self, ui: &mut egui::Ui, check_on_start: &mut bool) -> bool {
        ui.label(RichText::new(format!("Digimon Worlds Re:Compiled v{RUNNING}")).strong());
        let changed = ui.checkbox(check_on_start, "Check for a new version when the launcher starts").changed();
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(self.checking.is_none(), egui::Button::new("Check now")).clicked() {
                self.check(true);
            }
            if self.checking.is_some() {
                ui.spinner();
                ui.label(RichText::new("Checking…").color(MUTED));
            } else if let Some(c) = &self.check {
                if let Some(rel) = c.update() {
                    ui.label(RichText::new(format!("Version {} is available.", rel.version)).color(ACCENT));
                    if ui.button("Update…").clicked() {
                        self.window = true;
                    }
                } else if let Some(e) = &c.error {
                    ui.label(RichText::new(format!("Could not check: {e}")).color(BAD));
                } else {
                    let ago = dwrc_modhub::cache::now().saturating_sub(c.checked_at);
                    let when = match ago {
                        0..=59 => "just now".to_string(),
                        60..=3599 => format!("{} min ago", ago / 60),
                        _ => format!("{} h ago", ago / 3600),
                    };
                    ui.label(RichText::new(format!("You have the latest version (checked {when}).")).color(OK));
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.hyperlink_to("Source code and releases", GITHUB_URL);
            ui.label(RichText::new("·").color(MUTED));
            ui.hyperlink_to("Discord", DISCORD_URL);
            ui.label(RichText::new("·").color(MUTED));
            ui.hyperlink_to("Ko-fi", KOFI_URL);
        });
        changed
    }
}

/// The Games page footer. Returns a message to show.
pub fn footer(ctx: &egui::Context, updater: &mut Updater) -> Option<String> {
    let mut msg = None;
    egui::TopBottomPanel::bottom("footer")
        .frame(egui::Frame::default().fill(PANEL_BG).inner_margin(egui::Margin::symmetric(18, 10)))
        .show(ctx, |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                msg = updater.footer_left(ui);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, Icon::Heart, "Support on Ko-fi", KOFI_FILL).on_hover_text(KOFI_URL).clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(KOFI_URL));
                    }
                    if icon_button(ui, Icon::Chat, "Join the Discord", DISCORD_FILL).on_hover_text(DISCORD_URL).clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(DISCORD_URL));
                    }
                });
            });
        });
    msg
}

#[derive(Clone, Copy)]
enum Icon {
    Chat,
    Heart,
    Download,
    Restart,
}

/// A filled button with a small drawn icon (drawn, so it never depends on
/// which symbols the font has).
fn icon_button(ui: &mut egui::Ui, icon: Icon, label: &str, fill: Color32) -> egui::Response {
    let font = FontId::proportional(14.5);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, Color32::WHITE);
    let (icon_size, gap, pad) = (16.0, 8.0, Vec2::new(12.0, 7.0));
    let size = Vec2::new(pad.x * 2.0 + icon_size + gap + galley.size().x, pad.y * 2.0 + galley.size().y.max(icon_size));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if resp.is_pointer_button_down_on() {
            scale(fill, 0.85)
        } else if resp.hovered() {
            scale(fill, 1.15)
        } else {
            fill
        };
        let p = ui.painter();
        p.rect_filled(rect, 6.0, fill);
        let icon_rect = Rect::from_center_size(Pos2::new(rect.left() + pad.x + icon_size / 2.0, rect.center().y), Vec2::splat(icon_size));
        draw_icon(p, icon, icon_rect, fill);
        p.galley(Pos2::new(icon_rect.right() + gap, rect.center().y - galley.size().y / 2.0), galley, Color32::WHITE);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn scale(c: Color32, f: f32) -> Color32 {
    let s = |v: u8| (v as f32 * f).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(s(c.r()), s(c.g()), s(c.b()))
}

fn draw_icon(p: &egui::Painter, icon: Icon, r: Rect, bg: Color32) {
    let w = Color32::WHITE;
    let c = r.center();
    let u = r.width() / 16.0;
    match icon {
        Icon::Chat => {
            // A speech bubble with three dots.
            let body = Rect::from_min_max(Pos2::new(r.left(), r.top() + u), Pos2::new(r.right(), r.bottom() - 3.5 * u));
            p.rect_filled(body, 4.0 * u, w);
            p.add(epaint::PathShape::convex_polygon(
                vec![Pos2::new(r.left() + 3.0 * u, body.bottom() - 0.5), Pos2::new(r.left() + 7.5 * u, body.bottom() - 0.5), Pos2::new(r.left() + 3.0 * u, r.bottom())],
                w,
                Stroke::NONE,
            ));
            for dx in [-3.8, 0.0, 3.8] {
                p.circle_filled(Pos2::new(c.x + dx * u, body.center().y), 1.3 * u, bg);
            }
        }
        Icon::Heart => {
            let rad = 3.9 * u;
            let cy = r.top() + 5.6 * u;
            p.circle_filled(Pos2::new(c.x - 3.5 * u, cy), rad, w);
            p.circle_filled(Pos2::new(c.x + 3.5 * u, cy), rad, w);
            p.add(epaint::PathShape::convex_polygon(
                vec![Pos2::new(c.x - 7.25 * u, cy + 1.2 * u), Pos2::new(c.x + 7.25 * u, cy + 1.2 * u), Pos2::new(c.x, r.bottom() - 0.5 * u)],
                w,
                Stroke::NONE,
            ));
        }
        Icon::Download => {
            // An arrow onto a tray.
            let s = Stroke::new(2.0 * u, w);
            p.line_segment([Pos2::new(c.x, r.top() + u), Pos2::new(c.x, r.top() + 10.0 * u)], s);
            p.add(epaint::PathShape::convex_polygon(
                vec![Pos2::new(c.x - 4.5 * u, r.top() + 7.0 * u), Pos2::new(c.x + 4.5 * u, r.top() + 7.0 * u), Pos2::new(c.x, r.top() + 12.0 * u)],
                w,
                Stroke::NONE,
            ));
            p.line_segment([Pos2::new(r.left() + u, r.bottom() - 1.5 * u), Pos2::new(r.right() - u, r.bottom() - 1.5 * u)], s);
        }
        Icon::Restart => {
            // A circular arrow.
            let s = Stroke::new(2.0 * u, w);
            let rad = 5.5 * u;
            let pts: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let a = -1.2 + (i as f32 / 24.0) * 5.0;
                    Pos2::new(c.x + rad * a.cos(), c.y + rad * a.sin())
                })
                .collect();
            let end = *pts.last().unwrap();
            p.add(epaint::PathShape::line(pts, s));
            p.add(epaint::PathShape::convex_polygon(
                vec![Pos2::new(end.x - 3.0 * u, end.y - 0.5 * u), Pos2::new(end.x + 3.2 * u, end.y - 1.5 * u), Pos2::new(end.x + 0.6 * u, end.y + 3.5 * u)],
                w,
                Stroke::NONE,
            ));
        }
    }
}
