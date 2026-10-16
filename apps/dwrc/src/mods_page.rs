//! The Mods page (a top-level page beside Games and Settings), modelled on
//! Gen1Recomp's launcher:
//!
//! * **Installed** – every installed mod in one shared load order, switched
//!   on per game, with options, update checks against each mod's GitHub
//!   releases, a version picker (update or roll back), uninstall, safe mode.
//! * **Find mods** – browse mod indexes (the built-in Re:Compiled index
//!   plus any the player adds) and install from them.
//! * **Import .zip** – install a mod from a file (or drop it on the window).
//!
//! Network and disk work runs on worker threads; results come back as
//! messages and are applied on the UI thread.

use crate::app::{ACCENT, ACCENT_DIM, BAD, BLUE, CARD_BG, MUTED, OK, WARN};
use dwrc_disc::GameId;
use dwrc_modhub::index::{self, CompatContext, SortKey};
use dwrc_modhub::{text, Hub, IndexEntry, IndexFetch, IndexSource, Progress, Release, ReleasesFetch, UpdateOffer};
use dwrc_mods::package::{self, AlreadyInstalled, InstallOptions, Installed};
use dwrc_mods::{ModInfo, ModSource, OptionKind, Skip};
use dwrc_runtime::{Config, Library, Paths};
use eframe::egui::{self, Color32, RichText, Stroke, TextureHandle, TextureOptions, Vec2};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// An installed mod's card under the pointer (the card opens on a click).
const CARD_HOVER: Color32 = Color32::from_rgb(35, 39, 48);

pub fn game_short(g: GameId) -> &'static str {
    match g {
        GameId::Dw1 => "DW1",
        GameId::Dw2 => "DW2",
        GameId::Dw3 => "DW3",
    }
}

pub fn game_color(g: GameId) -> Color32 {
    match g {
        GameId::Dw1 => ACCENT,
        GameId::Dw2 => BLUE,
        GameId::Dw3 => Color32::from_rgb(120, 200, 110),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Installed,
    Find,
}

/// Everything the page needs from the app for one frame.
pub struct Env<'a> {
    pub cfg: &'a mut Config,
    pub paths: &'a Paths,
    pub lib: &'a Library,
    /// The game being played, if any (changes then need a restart).
    pub running: Option<GameId>,
}

enum Remote<T> {
    Loading,
    Ready(T),
}

enum Msg {
    Index(String, IndexFetch),
    Releases(String, ReleasesFetch),
    /// The releases of several mods published from one repository.
    ReleaseBatch(Vec<(String, ReleasesFetch)>),
    Text(String, Result<String, String>),
    Image(String, Result<(u32, u32, Vec<u8>), String>),
    Installed(Vec<Result<Installed, String>>),
    AskReplace(PathBuf, AlreadyInstalled),
}

/// Where an install comes from.
#[derive(Clone)]
enum Source {
    File(PathBuf),
    Url { url: String, id: String, label: String },
}

struct Op {
    label: String,
    progress: Arc<Progress>,
}

enum Confirm {
    Uninstall { id: String, name: String, dir: PathBuf },
    Replace { zip: PathBuf, info: AlreadyInstalled },
    IndexInstall { entry: Box<IndexEntry>, url: String, label: String, warnings: Vec<String> },
}

/// Something the user did this frame, applied after drawing.
enum Act {
    Toggle(String, GameId, bool),
    Move(String, i32),
    Edge(String, bool),
    SetOption(String, String, toml::Value),
    ResetOptions(String),
    Expand(String),
    AskUninstall(String),
    Install(Source, bool),
    UpdateAll,
    CheckUpdates,
    Versions(String),
    EnableAll(bool),
    ResetOrder,
    OpenFolder(PathBuf),
    Details(String, String),
    InstallListing(String, String),
    AddIndex,
    RemoveIndex(String),
    RefreshIndexes,
    /// Settings changed outside the other actions.
    Dirty,
}

pub struct ModsUi {
    hub: Hub,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    pub tab: Tab,
    mods: Vec<ModInfo>,
    broken: Vec<(PathBuf, String)>,
    loaded: bool,
    pub search: String,
    pub game_filter: Option<GameId>,
    expanded: Option<String>,
    releases: HashMap<String, Remote<ReleasesFetch>>,
    checked_updates: bool,
    /// When the indexes and mod updates were last asked for live: at
    /// start-up, then every few hours while the launcher stays open.
    last_live: Option<Instant>,
    sources: Vec<IndexSource>,
    indexes: HashMap<String, Remote<IndexFetch>>,
    find_source: Option<String>,
    find_search: String,
    find_game: Option<GameId>,
    find_category: Option<String>,
    find_sort: SortKey,
    details: Option<(String, String)>,
    texts: HashMap<String, Remote<Result<String, String>>>,
    images: HashMap<String, Remote<Option<TextureHandle>>>,
    op: Option<Op>,
    confirm: Option<Confirm>,
    versions_for: Option<String>,
    show_indexes: bool,
    new_index: String,
    index_error: Option<String>,
    /// Messages for the app's toast.
    pub toasts: Vec<(String, bool)>,
    /// Set when installed mods or their settings changed while a game runs.
    pub changed_while_running: bool,
}

impl ModsUi {
    pub fn new(paths: &Paths) -> Self {
        let (tx, rx) = channel();
        package::clean_leftovers(&paths.user_mods_root());
        Self {
            hub: Hub::new(paths.modhub_cache_dir()),
            tx,
            rx,
            tab: Tab::Installed,
            mods: Vec::new(),
            broken: Vec::new(),
            loaded: false,
            search: String::new(),
            game_filter: None,
            expanded: None,
            releases: HashMap::new(),
            checked_updates: false,
            last_live: None,
            sources: Vec::new(),
            indexes: HashMap::new(),
            find_source: None,
            find_search: String::new(),
            find_game: None,
            find_category: None,
            find_sort: SortKey::Name,
            details: None,
            texts: HashMap::new(),
            images: HashMap::new(),
            op: None,
            confirm: None,
            versions_for: None,
            show_indexes: false,
            new_index: String::new(),
            index_error: None,
            toasts: Vec::new(),
            changed_while_running: false,
        }
    }

    // ------------------------------------------------------------------
    // Data
    // ------------------------------------------------------------------

    pub fn reload(&mut self, paths: &Paths) {
        self.mods = dwrc_runtime::installed_mods(paths);
        self.broken = dwrc_mods::broken_mods(&paths.user_mods_root());
        self.loaded = true;
    }

    fn ensure_loaded(&mut self, env: &Env) {
        if !self.loaded {
            self.reload(env.paths);
        }
        let want = index::sources(&env.cfg.mods.indexes);
        if want != self.sources {
            self.sources = want;
        }
    }

    /// Installed mods that load for a game (for the game cards).
    pub fn active_count(&mut self, env: &Env, g: GameId) -> usize {
        self.ensure_loaded(env);
        dwrc_runtime::mod_plan(env.cfg, &self.mods, g, None).0.len()
    }

    fn spawn(&self, ctx: &egui::Context, f: impl FnOnce(&Hub) -> Msg + Send + 'static) {
        let (tx, hub, ctx) = (self.tx.clone(), self.hub.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(f(&hub));
            ctx.request_repaint();
        });
    }

    /// Called every frame: refreshes the mod indexes from the internet when
    /// the launcher starts (a cached copy could predate mods added or updated
    /// since) and checks installed mods for updates (if the player keeps that
    /// on), then again every few hours while it stays open.
    pub fn tick(&mut self, ctx: &egui::Context, env: &Env) {
        let due = self.last_live.is_none_or(|t| t.elapsed() >= Duration::from_secs(dwrc_modhub::RELEASES_TTL));
        if !due || self.op.is_some() {
            return;
        }
        self.ensure_loaded(env);
        self.last_live = Some(Instant::now());
        self.fetch_indexes(ctx, true);
        if env.cfg.mods.check_updates {
            self.check_updates(ctx, true);
        }
    }

    /// How many installed mods have an update waiting.
    pub fn update_count(&self) -> usize {
        self.mods.iter().filter(|m| self.offer(m).is_some()).count()
    }

    fn fetch_indexes(&mut self, ctx: &egui::Context, force: bool) {
        for s in self.sources.clone() {
            match self.indexes.get(&s.feed) {
                Some(Remote::Loading) => continue,
                Some(_) if !force => continue,
                _ => {}
            }
            self.indexes.insert(s.feed.clone(), Remote::Loading);
            self.spawn(ctx, move |hub| Msg::Index(s.feed.clone(), hub.fetch_index(&s, force)));
        }
    }

    fn check_updates(&mut self, ctx: &egui::Context, force: bool) {
        // One request per repository: an index hosting many mods is asked once.
        let mut by_repo: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for m in &self.mods {
            if m.source != ModSource::User {
                continue;
            }
            let Some(repo) = m.manifest.info.github_repo() else { continue };
            match self.releases.get(m.id()) {
                Some(Remote::Loading) => continue,
                Some(_) if !force => continue,
                _ => {}
            }
            by_repo.entry(repo).or_default().push(m.id().to_string());
        }
        for (repo, ids) in by_repo {
            for id in &ids {
                self.releases.insert(id.clone(), Remote::Loading);
            }
            self.spawn(ctx, move |hub| Msg::ReleaseBatch(hub.fetch_releases_many(&repo, &ids, force)));
        }
        self.checked_updates = true;
    }

    fn want_text(&mut self, ctx: &egui::Context, url: &str) {
        if self.texts.contains_key(url) {
            return;
        }
        self.texts.insert(url.to_string(), Remote::Loading);
        let u = url.to_string();
        self.spawn(ctx, move |hub| Msg::Text(u.clone(), hub.fetch_text(&u).map_err(|e| format!("{e:#}"))));
    }

    fn want_image(&mut self, ctx: &egui::Context, url: &str) {
        if self.images.contains_key(url) {
            return;
        }
        self.images.insert(url.to_string(), Remote::Loading);
        let u = url.to_string();
        self.spawn(ctx, move |hub| Msg::Image(u.clone(), hub.fetch_image(&u).map_err(|e| format!("{e:#}")).and_then(|b| decode_png(&b))));
    }

    /// Every listing for a mod id across the loaded indexes.
    fn listings(&self, id: &str) -> Vec<(&IndexSource, &IndexEntry)> {
        let mut out = Vec::new();
        for s in &self.sources {
            if let Some(Remote::Ready(f)) = self.indexes.get(&s.feed) {
                if let Some(ix) = &f.index {
                    if let Some(e) = ix.mods.iter().find(|e| e.id == id) {
                        out.push((s, e));
                    }
                }
            }
        }
        out
    }

    fn offer(&self, m: &ModInfo) -> Option<UpdateOffer> {
        if m.source != ModSource::User {
            return None;
        }
        let rel = match self.releases.get(m.id()) {
            Some(Remote::Ready(r)) => Some(r.releases.as_slice()),
            _ => None,
        };
        dwrc_modhub::update_offer(m.version(), rel, &self.listings(m.id()))
    }

    // ------------------------------------------------------------------
    // Installing
    // ------------------------------------------------------------------

    fn start_install(&mut self, ctx: &egui::Context, env: &Env, jobs: Vec<(Source, bool)>) {
        if self.op.is_some() || jobs.is_empty() {
            return;
        }
        let progress = Arc::new(Progress::default());
        let label = match &jobs[..] {
            [(Source::File(p), _)] => format!("Installing {}", p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()),
            [(Source::Url { label, .. }, _)] => label.clone(),
            many => format!("Updating {} mods", many.len()),
        };
        self.op = Some(Op { label, progress: progress.clone() });
        let mods_root = env.paths.user_mods_root();
        let dl_dir = env.paths.downloads_dir();
        let (tx, hub, ctx) = (self.tx.clone(), self.hub.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut results = Vec::new();
            for (src, replace) in jobs {
                let r = match &src {
                    Source::File(path) => match package::install_zip(path, &mods_root, &InstallOptions { expect_id: None, replace }) {
                        Err(e) if e.downcast_ref::<AlreadyInstalled>().is_some() => {
                            let info = e.downcast::<AlreadyInstalled>().unwrap();
                            let _ = tx.send(Msg::AskReplace(path.clone(), info));
                            continue;
                        }
                        r => r.map_err(|e| format!("{e:#}")),
                    },
                    Source::Url { url, id, .. } => match hub.download_zip(url, &dl_dir, &progress) {
                        Ok(zip) => {
                            let opts = InstallOptions { expect_id: Some(id.clone()), replace: true };
                            let r = package::install_zip(&zip, &mods_root, &opts).map_err(|e| format!("{e:#}"));
                            let _ = std::fs::remove_file(&zip);
                            r
                        }
                        Err(e) => Err(format!("download failed: {e:#}")),
                    },
                };
                results.push(r);
            }
            let _ = tx.send(Msg::Installed(results));
            ctx.request_repaint();
        });
    }

    /// Installs a dropped or chosen `.zip`.
    pub fn import_zip(&mut self, ctx: &egui::Context, env: &Env, path: PathBuf) {
        self.tab = Tab::Installed;
        self.start_install(ctx, env, vec![(Source::File(path), false)]);
    }

    fn after_install(&mut self, env: &mut Env, results: Vec<Result<Installed, String>>) {
        let mut ok = Vec::new();
        let mut failed = Vec::new();
        for r in results {
            match r {
                Ok(i) => {
                    let games = i.manifest.info.games();
                    if i.replaced.is_none() {
                        env.cfg.mods.note_installed(&i.id, &games);
                    } else {
                        env.cfg.mods.ensure_ordered(&i.id);
                    }
                    let what = match &i.replaced {
                        Some(old) if dwrc_mods::version::compare(&i.version, old).is_lt() => format!("{} rolled back to v{} (was v{old})", i.name, i.version),
                        Some(old) if old != &i.version => format!("{} updated to v{} (was v{old})", i.name, i.version),
                        Some(_) => format!("{} v{} reinstalled", i.name, i.version),
                        None => {
                            let on: Vec<&str> = games.iter().map(|g| game_short(*g)).collect();
                            format!("{} v{} installed and switched on for {}", i.name, i.version, on.join(", "))
                        }
                    };
                    ok.push(what);
                }
                Err(e) => failed.push(e),
            }
        }
        if !ok.is_empty() {
            self.reload(env.paths);
            if env.running.is_some() {
                self.changed_while_running = true;
            }
            self.toasts.push((ok.join("\n"), false));
        }
        for e in failed {
            self.toasts.push((e, true));
        }
    }

    // ------------------------------------------------------------------
    // Frame
    // ------------------------------------------------------------------

    /// Applies finished background work. Returns true if settings changed.
    pub fn poll(&mut self, ctx: &egui::Context, env: &mut Env) -> bool {
        let mut cfg_changed = false;
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Index(feed, f) => {
                    self.indexes.insert(feed, Remote::Ready(f));
                }
                Msg::Releases(id, f) => {
                    self.releases.insert(id, Remote::Ready(f));
                }
                Msg::ReleaseBatch(list) => {
                    for (id, f) in list {
                        self.releases.insert(id, Remote::Ready(f));
                    }
                }
                Msg::Text(url, r) => {
                    self.texts.insert(url, Remote::Ready(r));
                }
                Msg::Image(url, r) => {
                    let tex = r.ok().map(|(w, h, rgba)| {
                        ctx.load_texture(url.clone(), egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba), TextureOptions::LINEAR)
                    });
                    self.images.insert(url, Remote::Ready(tex));
                }
                Msg::AskReplace(zip, info) => {
                    self.confirm = Some(Confirm::Replace { zip, info });
                }
                Msg::Installed(results) => {
                    self.op = None;
                    if !results.is_empty() {
                        self.after_install(env, results);
                        cfg_changed = true;
                    }
                }
            }
        }
        cfg_changed
    }

    /// Draws the page into the central panel. Returns true if settings changed.
    pub fn show(&mut self, ui: &mut egui::Ui, env: &mut Env) -> bool {
        let ctx = ui.ctx().clone();
        ui.set_max_width(980.0);
        self.ensure_loaded(env);
        self.fetch_indexes(&ctx, false);
        if !self.checked_updates && env.cfg.mods.check_updates {
            self.check_updates(&ctx, false);
        }
        let mut acts: Vec<Act> = Vec::new();
        let mut changed = false;

        // Header: tabs and file actions.
        ui.horizontal_wrapped(|ui| {
            let n = self.mods.len();
            for (t, label) in [(Tab::Installed, format!("Installed ({n})")), (Tab::Find, "Find mods".to_string())] {
                let sel = self.tab == t;
                let txt = RichText::new(label).size(17.0).strong().color(if sel { Color32::WHITE } else { MUTED });
                if ui.add(egui::Button::new(txt).fill(if sel { ACCENT_DIM } else { Color32::TRANSPARENT }).min_size(Vec2::new(0.0, 32.0))).clicked() {
                    self.tab = t;
                }
            }
            ui.add_space(16.0);
            if ui.button("Import .zip…").on_hover_text("Install a mod from a .zip file (you can also drop it on the window)").clicked() {
                if let Some(p) = rfd::FileDialog::new().set_title("Choose a mod .zip").add_filter("Mod archive", &["zip", "ZIP"]).pick_file() {
                    acts.push(Act::Install(Source::File(p), false));
                }
            }
            if ui.button("Open mods folder").clicked() {
                acts.push(Act::OpenFolder(env.paths.user_mods_root()));
            }
            ui.menu_button("Options", |ui| {
                changed |= ui.checkbox(&mut env.cfg.mods.check_updates, "Check for mod updates automatically").changed();
                let safe = ui
                    .checkbox(&mut env.cfg.mods.safe_mode, "Safe mode (start games with no mods)")
                    .on_hover_text("Temporarily turns every mod off without forgetting which ones you use. Handy when tracking down a problem.");
                if safe.changed() {
                    changed = true;
                    if env.running.is_some() {
                        self.changed_while_running = true;
                    }
                }
                if ui.button("Reload installed mods").clicked() {
                    self.reload(env.paths);
                    ui.close_menu();
                }
            });
        });
        if let Some(op) = &self.op {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.label(&op.label);
                match op.progress.fraction() {
                    Some(f) => {
                        ui.add(egui::ProgressBar::new(f).desired_width(200.0).show_percentage());
                    }
                    None => {
                        let kb = op.progress.done.load(std::sync::atomic::Ordering::Relaxed) / 1024;
                        if kb > 0 {
                            ui.label(RichText::new(format!("{kb} KB")).color(MUTED));
                        }
                    }
                }
                if ui.button("Cancel").clicked() {
                    op.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
        if env.cfg.mods.safe_mode {
            banner(ui, WARN, "Safe mode is on: games start without any mods. Your choices below are kept.", |ui| {
                if ui.button("Turn off").clicked() {
                    env.cfg.mods.safe_mode = false;
                    changed = true;
                }
            });
        }
        if self.changed_while_running {
            if let Some(g) = env.running {
                banner(ui, BLUE, &format!("Mod changes apply when {} restarts.", g.title()), |_| {});
            }
        }
        ui.add_space(6.0);
        match self.tab {
            Tab::Installed => self.installed_tab(ui, env, &mut acts),
            Tab::Find => self.find_tab(ui, env, &mut acts),
        }
        self.windows(&ctx, env, &mut acts);
        changed |= self.apply(&ctx, env, acts);
        changed
    }

    // ------------------------------------------------------------------
    // Installed tab
    // ------------------------------------------------------------------

    fn installed_tab(&mut self, ui: &mut egui::Ui, env: &mut Env, acts: &mut Vec<Act>) {
        // Update summary.
        let user_with_repo = self.mods.iter().filter(|m| m.source == ModSource::User && m.manifest.info.github_repo().is_some()).count();
        let offers: Vec<(String, UpdateOffer)> = self.mods.iter().filter_map(|m| self.offer(m).map(|o| (m.id().to_string(), o))).collect();
        let checking = self.releases.values().any(|r| matches!(r, Remote::Loading));
        ui.horizontal_wrapped(|ui| {
            if checking {
                ui.add(egui::Spinner::new());
                ui.label(RichText::new("Checking for updates…").color(MUTED));
            } else if !offers.is_empty() {
                ui.label(RichText::new(format!("{} update{} available", offers.len(), if offers.len() == 1 { "" } else { "s" })).color(OK).strong());
                if ui.add_enabled(self.op.is_none(), egui::Button::new("Update all")).clicked() {
                    acts.push(Act::UpdateAll);
                }
            } else if user_with_repo > 0 && self.checked_updates {
                let newest = self.releases.values().filter_map(|r| if let Remote::Ready(f) = r { Some(f.checked_at) } else { None }).max();
                let when = newest.filter(|t| *t > 0).map(|t| format!(" · checked {}", text::ago(t))).unwrap_or_default();
                ui.label(RichText::new(format!("Mods are up to date{when}")).color(MUTED));
            }
            if user_with_repo > 0 && !checking && ui.button("Check for updates").clicked() {
                acts.push(Act::CheckUpdates);
            }
            let errors: Vec<String> = self
                .releases
                .iter()
                .filter_map(|(id, r)| if let Remote::Ready(f) = r { f.error.as_ref().map(|e| format!("{id}: {e}")) } else { None })
                .collect();
            if !errors.is_empty() {
                ui.label(RichText::new(format!("⚠ {} update check{} failed", errors.len(), if errors.len() == 1 { "" } else { "s" })).color(WARN))
                    .on_hover_text(errors.join("\n"));
            }
        });
        // Toolbar.
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search mods").desired_width(200.0));
            ui.label("Show for");
            egui::ComboBox::from_id_salt("mods-game")
                .selected_text(self.game_filter.map_or("All games", |g| g.title()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.game_filter, None, "All games");
                    for g in GameId::ALL {
                        ui.selectable_value(&mut self.game_filter, Some(g), g.title());
                    }
                });
            let scope = self.game_filter.map_or("every game".to_string(), |g| g.title().to_string());
            if ui.button("Enable all").on_hover_text(format!("Switch on every listed mod for {scope}")).clicked() {
                acts.push(Act::EnableAll(true));
            }
            if ui.button("Disable all").on_hover_text(format!("Switch off every listed mod for {scope}")).clicked() {
                acts.push(Act::EnableAll(false));
            }
            if ui.button("Reset load order").on_hover_text("Sort the load order by mod id").clicked() {
                acts.push(Act::ResetOrder);
            }
        });
        ui.label(
            RichText::new("Mods load top to bottom; when two change the same thing, the lower one wins. Switch each mod on for the games you want.")
                .color(MUTED)
                .small(),
        );
        ui.add_space(4.0);
        let order = env.cfg.mods.load_order(self.mods.iter().map(|m| m.id()));
        let visible: Vec<String> = order
            .iter()
            .filter(|id| {
                let Some(m) = self.mods.iter().find(|m| m.id() == id.as_str()) else { return false };
                let q = self.search.to_lowercase();
                let hit = q.split_whitespace().all(|t| {
                    format!("{} {} {} {}", m.name(), m.id(), m.manifest.info.author, m.manifest.info.description).to_lowercase().contains(t)
                });
                hit && self.game_filter.is_none_or(|g| m.supports(g))
            })
            .cloned()
            .collect();
        // Why enabled mods would not load, per game.
        let mut skips: HashMap<String, Vec<(GameId, Skip)>> = HashMap::new();
        for g in GameId::ALL {
            for (m, why) in dwrc_runtime::mod_plan(env.cfg, &self.mods, g, None).1 {
                skips.entry(m.id().to_string()).or_default().push((g, why));
            }
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(980.0);
            if self.mods.is_empty() {
                ui.add_space(20.0);
                ui.label(RichText::new("No mods installed yet.").size(16.0));
                ui.label(RichText::new("Find some in the Find mods tab, or import a mod .zip.").color(MUTED));
            } else if visible.is_empty() {
                ui.label(RichText::new("No mods match.").color(MUTED));
            }
            for (k, id) in visible.iter().enumerate() {
                let Some(m) = self.mods.iter().find(|m| m.id() == id.as_str()) else { continue };
                let rank = order.iter().position(|o| o == id).unwrap_or(0) + 1;
                let offer = self.offer(m);
                self.mod_row(ui, env, m, rank, k, visible.len(), offer, skips.get(id.as_str()), acts);
            }
            for (dir, err) in &self.broken {
                egui::Frame::default().fill(CARD_BG).corner_radius(8.0).inner_margin(10.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(format!("✖ {} could not be read", dir.file_name().unwrap_or_default().to_string_lossy())).color(BAD).strong());
                    ui.label(RichText::new(err).color(MUTED).small());
                    if ui.button("Open folder").clicked() {
                        acts.push(Act::OpenFolder(dir.clone()));
                    }
                });
                ui.add_space(6.0);
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn mod_row(
        &self,
        ui: &mut egui::Ui,
        env: &Env,
        m: &ModInfo,
        rank: usize,
        k: usize,
        shown: usize,
        offer: Option<UpdateOffer>,
        skips: Option<&Vec<(GameId, Skip)>>,
        acts: &mut Vec<Act>,
    ) {
        let id = m.id().to_string();
        let info = &m.manifest.info;
        let expanded = self.expanded.as_deref() == Some(id.as_str());
        let any_on = GameId::ALL.iter().any(|g| env.cfg.mods.is_enabled(*g, &id) && m.supports(*g));
        // The whole card opens and closes the details; its buttons still work
        // on their own (the card's click is registered beneath them).
        let card = ui.scope_builder(egui::UiBuilder::new().id_salt(("mod-card", id.as_str())).sense(egui::Sense::click()), |ui| {
            let hover = ui.response().contains_pointer();
            // Text in the card passes clicks to the card rather than selecting.
            ui.style_mut().interaction.selectable_labels = false;
            let stroke = if expanded { Stroke::new(1.0_f32, ACCENT_DIM) } else if hover { Stroke::new(1.0_f32, Color32::from_gray(64)) } else { Stroke::NONE };
            let fill = if hover && !expanded { CARD_HOVER } else { CARD_BG };
            egui::Frame::default().fill(fill).stroke(stroke).corner_radius(8.0).inner_margin(10.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{rank:>2}")).monospace().color(MUTED));
                    ui.label(RichText::new(m.name()).size(16.0).strong().color(if any_on { Color32::WHITE } else { MUTED }));
                    ui.label(RichText::new(format!("v{}", info.version)).color(MUTED));
                    if !info.author.is_empty() {
                        ui.label(RichText::new(format!("by {}", info.author)).color(MUTED));
                    }
                    for c in info.categories.iter().take(2) {
                        tag(ui, &c.to_uppercase(), MUTED);
                    }
                    if m.source == ModSource::Bundled {
                        tag(ui, "BUILT-IN", BLUE);
                    }
                    if info.experimental {
                        tag(ui, "EXPERIMENTAL", WARN);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button("More", |ui| {
                            if ui.button("Move to top").clicked() {
                                acts.push(Act::Edge(id.clone(), true));
                                ui.close_menu();
                            }
                            if ui.button("Move to bottom").clicked() {
                                acts.push(Act::Edge(id.clone(), false));
                                ui.close_menu();
                            }
                            if info.github_repo().is_some() && m.source == ModSource::User && ui.button("Versions…").clicked() {
                                acts.push(Act::Versions(id.clone()));
                                ui.close_menu();
                            }
                            if ui.button("Open folder").clicked() {
                                acts.push(Act::OpenFolder(m.root.clone()));
                                ui.close_menu();
                            }
                            if !m.manifest.options.is_empty() && ui.button("Reset options").clicked() {
                                acts.push(Act::ResetOptions(id.clone()));
                                ui.close_menu();
                            }
                            if m.source == ModSource::User {
                                ui.separator();
                                if ui.button(RichText::new("Uninstall…").color(BAD)).clicked() {
                                    acts.push(Act::AskUninstall(id.clone()));
                                    ui.close_menu();
                                }
                            }
                        });
                        if ui.add_enabled(k + 1 < shown, egui::Button::new("Down")).clicked() {
                            acts.push(Act::Move(id.clone(), 1));
                        }
                        if ui.add_enabled(k > 0, egui::Button::new("Up")).clicked() {
                            acts.push(Act::Move(id.clone(), -1));
                        }
                        if let Some(o) = &offer {
                            let mut r = ui.add_enabled_ui(self.op.is_none(), |ui| crate::footer::update_button(ui, &format!("Update to v{}", o.version))).inner;
                            r = match &o.notes {
                                Some(n) => r.on_hover_text(format!("What's new:\n{}", text::clean_markdown(n, 600))),
                                None => r.on_hover_text(format!("Install {} v{}", m.name(), o.version)),
                            };
                            if r.clicked() {
                                acts.push(Act::Install(Source::Url { url: o.url.clone(), id: id.clone(), label: format!("Updating {}", m.name()) }, true));
                            }
                        }
                    });
                });
                ui.horizontal_wrapped(|ui| {
                    for g in GameId::ALL {
                        if !m.supports(g) {
                            continue;
                        }
                        let on = env.cfg.mods.is_enabled(g, &id);
                        let col = game_color(g);
                        let txt = RichText::new(format!("{} {}", if on { "✔" } else { "  " }, game_short(g))).strong().color(if on { Color32::BLACK } else { col });
                        let b = egui::Button::new(txt).fill(if on { col } else { Color32::TRANSPARENT }).stroke(Stroke::new(1.0_f32, col)).min_size(Vec2::new(58.0, 22.0));
                        let hover = format!("{} for {}", if on { "On" } else { "Off" }, g.title());
                        if ui.add_enabled(m.is_usable(), b).on_hover_text(hover).clicked() {
                            acts.push(Act::Toggle(id.clone(), g, !on));
                        }
                    }
                    if !info.description.is_empty() && !expanded {
                        ui.label(RichText::new(text::preview_line(&info.description, 110)).color(MUTED));
                    }
                });
                for p in &m.problems {
                    ui.label(RichText::new(format!("✖ {p}")).color(BAD));
                }
                if let Some(list) = skips {
                    for (g, why) in list {
                        if *why != Skip::WrongGame {
                            ui.label(RichText::new(format!("⚠ Not loading in {}: {why}", game_short(*g))).color(WARN));
                        }
                    }
                }
                if expanded {
                    ui.separator();
                    // Clicks among the details and options stay there (they
                    // don't close the card), and its text can be selected.
                    ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
                        ui.style_mut().interaction.selectable_labels = true;
                        self.mod_details(ui, env, m, offer.as_ref(), acts);
                    });
                }
            });
        });
        let card = card.response.on_hover_cursor(egui::CursorIcon::PointingHand);
        if card.clicked() {
            acts.push(Act::Expand(id.clone()));
        }
        ui.add_space(6.0);
    }

    fn mod_details(&self, ui: &mut egui::Ui, env: &Env, m: &ModInfo, offer: Option<&UpdateOffer>, acts: &mut Vec<Act>) {
        let info = &m.manifest.info;
        if !info.description.is_empty() {
            ui.label(&info.description);
            ui.add_space(4.0);
        }
        egui::Grid::new(format!("details-{}", info.id)).num_columns(2).spacing([12.0, 3.0]).show(ui, |ui| {
            let row = |ui: &mut egui::Ui, k: &str, v: &str| {
                ui.label(RichText::new(k).color(MUTED));
                ui.label(v);
                ui.end_row();
            };
            row(ui, "Id", &info.id);
            row(ui, "Games", &info.games().iter().map(|g| g.title()).collect::<Vec<_>>().join(", "));
            if !info.serials.is_empty() {
                row(ui, "Releases", &info.serials.join(", "));
            }
            if let Some(l) = &info.license {
                row(ui, "Licence", l);
            }
            if !info.dependencies.is_empty() {
                row(ui, "Needs", &info.dependencies.join(", "));
            }
            if !info.conflicts.is_empty() {
                row(ui, "Conflicts with", &info.conflicts.join(", "));
            }
            if let Some(r) = &info.engine {
                row(ui, "Re:Compiled", r);
            }
            ui.label(RichText::new("Updates").color(MUTED));
            match info.github_repo() {
                Some(repo) => {
                    ui.hyperlink_to(format!("github.com/{repo}"), format!("https://github.com/{repo}"));
                }
                None => {
                    ui.label(RichText::new(if m.source == ModSource::Bundled { "ships with Re:Compiled" } else { "no GitHub repository set" }).color(MUTED));
                }
            }
            ui.end_row();
            if let Some(h) = &info.homepage {
                ui.label(RichText::new("Website").color(MUTED));
                ui.hyperlink(h);
                ui.end_row();
            }
            row(ui, "Folder", &m.root.display().to_string());
        });
        if let Some(o) = offer {
            if let Some(n) = &o.notes {
                egui::CollapsingHeader::new(format!("What's new in v{}", o.version)).default_open(true).show(ui, |ui| {
                    ui.label(text::clean_markdown(n, 2000));
                });
            }
        }
        if let Some(Remote::Ready(f)) = self.releases.get(&info.id) {
            if let Some(e) = &f.error {
                ui.label(RichText::new(format!("⚠ Update check: {e}")).color(WARN).small());
            }
        }
        if !m.manifest.options.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new("Options").strong().color(BLUE));
            options_editor(ui, &info.id, &m.manifest.options, env.cfg.mods.options.get(&info.id), acts);
        }
    }

    // ------------------------------------------------------------------
    // Find mods tab
    // ------------------------------------------------------------------

    fn find_tab(&mut self, ui: &mut egui::Ui, env: &mut Env, acts: &mut Vec<Act>) {
        let ctx = ui.ctx().clone();
        ui.horizontal_wrapped(|ui| {
            ui.label("Browse");
            let cur = self.find_source.as_ref().and_then(|f| self.sources.iter().find(|s| &s.feed == f)).map(|s| s.label.clone());
            egui::ComboBox::from_id_salt("find-source").selected_text(cur.unwrap_or_else(|| "All indexes".into())).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.find_source, None, "All indexes");
                for s in &self.sources {
                    ui.selectable_value(&mut self.find_source, Some(s.feed.clone()), &s.label);
                }
            });
            if ui.button("Indexes…").on_hover_text("Add or remove mod indexes").clicked() {
                self.show_indexes = true;
            }
            ui.add(egui::TextEdit::singleline(&mut self.find_search).hint_text("Search mods").desired_width(180.0));
            egui::ComboBox::from_id_salt("find-game").selected_text(self.find_game.map_or("Any game", |g| g.title())).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.find_game, None, "Any game");
                for g in GameId::ALL {
                    ui.selectable_value(&mut self.find_game, Some(g), g.title());
                }
            });
            let mut cats: Vec<String> = Vec::new();
            for f in self.indexes.values() {
                if let Remote::Ready(IndexFetch { index: Some(ix), .. }) = f {
                    for e in &ix.mods {
                        for c in &e.categories {
                            if !cats.iter().any(|x| x.eq_ignore_ascii_case(c)) {
                                cats.push(c.to_uppercase());
                            }
                        }
                    }
                }
            }
            cats.sort();
            egui::ComboBox::from_id_salt("find-cat").selected_text(self.find_category.clone().unwrap_or_else(|| "Any category".into())).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.find_category, None, "Any category");
                for c in &cats {
                    ui.selectable_value(&mut self.find_category, Some(c.clone()), c);
                }
            });
            egui::ComboBox::from_id_salt("find-sort").selected_text(format!("Sort: {}", self.find_sort.label())).show_ui(ui, |ui| {
                for k in SortKey::ALL {
                    ui.selectable_value(&mut self.find_sort, k, k.label());
                }
            });
            let loading = self.indexes.values().any(|r| matches!(r, Remote::Loading));
            if ui.add_enabled(!loading, egui::Button::new("Refresh")).clicked() {
                acts.push(Act::RefreshIndexes);
            }
        });
        // Per-index status.
        for s in &self.sources {
            if self.find_source.as_ref().is_some_and(|f| f != &s.feed) {
                continue;
            }
            match self.indexes.get(&s.feed) {
                Some(Remote::Loading) | None => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui.label(RichText::new(format!("Loading {}…", s.label)).color(MUTED));
                    });
                }
                Some(Remote::Ready(f)) => {
                    if let Some(e) = &f.error {
                        let msg = if f.index.is_some() {
                            format!("⚠ {}: could not refresh ({e}); showing the copy from {}", s.label, text::ago(f.checked_at))
                        } else if s.builtin {
                            format!("⚠ {}: not reachable ({e}). The index may not be published yet, or you may be offline.", s.label)
                        } else {
                            format!("⚠ {}: {e}", s.label)
                        };
                        ui.label(RichText::new(msg).color(WARN).small());
                    }
                }
            }
        }
        // Entries.
        let mut entries: Vec<(&IndexSource, &IndexEntry, String)> = Vec::new();
        for s in &self.sources {
            if self.find_source.as_ref().is_some_and(|f| f != &s.feed) {
                continue;
            }
            if let Some(Remote::Ready(IndexFetch { index: Some(ix), base, .. })) = self.indexes.get(&s.feed) {
                for e in &ix.mods {
                    if entries.iter().any(|(_, x, _)| x.id == e.id) {
                        continue;
                    }
                    entries.push((s, e, base.clone()));
                }
            }
        }
        let total = entries.len();
        entries.retain(|(_, e, _)| {
            e.matches(&self.find_search)
                && self.find_game.is_none_or(|g| e.game_ids().contains(&g))
                && self.find_category.as_ref().is_none_or(|c| e.categories.iter().any(|x| x.eq_ignore_ascii_case(c)))
        });
        {
            let mut refs: Vec<&IndexEntry> = entries.iter().map(|(_, e, _)| *e).collect();
            index::sort(&mut refs, self.find_sort);
            let pos = |e: &IndexEntry| refs.iter().position(|r| std::ptr::eq(*r, e)).unwrap_or(0);
            entries.sort_by_key(|(_, e, _)| pos(e));
        }
        let installed: HashMap<&str, &ModInfo> = self.mods.iter().map(|m| (m.id(), m)).collect();
        let mut want_images: Vec<String> = Vec::new();
        ui.add_space(4.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(980.0);
            if total == 0 && !self.indexes.values().any(|r| matches!(r, Remote::Loading)) {
                ui.add_space(12.0);
                ui.label(RichText::new("No mods listed yet.").size(16.0));
                ui.label(
                    RichText::new("Indexes list mods published on GitHub. Add another index with Indexes…, or install a .zip you downloaded with Import .zip….")
                        .color(MUTED),
                );
            } else if entries.is_empty() && total > 0 {
                ui.label(RichText::new("No mods match that search.").color(MUTED));
            }
            for (src, e, base) in &entries {
                let thumb = e.thumbnail.as_deref().and_then(|t| index::join_url(base, t));
                let tex = thumb.as_ref().and_then(|u| match self.images.get(u) {
                    Some(Remote::Ready(t)) => t.clone(),
                    Some(Remote::Loading) => None,
                    None => {
                        want_images.push(u.clone());
                        None
                    }
                });
                listing_card(ui, src, e, tex.as_ref(), installed.get(e.id.as_str()).copied(), self.op.is_none(), acts);
            }
        });
        for u in want_images {
            self.want_image(&ctx, &u);
        }
        let _ = env;
    }

    // ------------------------------------------------------------------
    // Windows (details, versions, indexes, confirmations)
    // ------------------------------------------------------------------

    fn windows(&mut self, ctx: &egui::Context, env: &mut Env, acts: &mut Vec<Act>) {
        // Listing details.
        if let Some((feed, id)) = self.details.clone() {
            let found = self.sources.iter().find(|s| s.feed == feed).cloned().and_then(|s| match self.indexes.get(&feed) {
                Some(Remote::Ready(IndexFetch { index: Some(ix), base, .. })) => ix.mods.iter().find(|e| e.id == id).cloned().map(|e| (s, e, base.clone())),
                _ => None,
            });
            match found {
                None => self.details = None,
                Some((src, e, base)) => {
                    let desc_url = e.description_url.as_deref().and_then(|d| index::join_url(&base, d));
                    if let Some(u) = &desc_url {
                        self.want_text(ctx, u);
                    }
                    let mut open = true;
                    let installed = self.mods.iter().find(|m| m.id() == e.id).cloned();
                    egui::Window::new(&e.title).open(&mut open).default_width(560.0).collapsible(false).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.screen_rect().center()).show(ctx, |ui| {
                        let v = e.display_version();
                        let v = if v == "?" { "not released yet".to_string() } else { format!("v{v}") };
                        ui.label(RichText::new(format!("{v} · by {} · from {}", e.author.as_deref().unwrap_or("unknown"), src.label)).color(MUTED));
                        ui.horizontal_wrapped(|ui| {
                            for g in e.game_ids() {
                                tag(ui, game_short(g), game_color(g));
                            }
                            for c in &e.categories {
                                tag(ui, &c.to_uppercase(), MUTED);
                            }
                        });
                        if let Some(d) = e.total_downloads() {
                            ui.label(RichText::new(format!("{} downloads{}", text::thousands(d), e.updated().map(|u| format!(" · updated {u}")).unwrap_or_default())).color(MUTED).small());
                        }
                        ui.separator();
                        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                            match desc_url.as_ref().and_then(|u| self.texts.get(u)) {
                                Some(Remote::Ready(Ok(t))) => {
                                    ui.label(text::clean_markdown(t, 6000));
                                }
                                Some(Remote::Loading) => {
                                    ui.add(egui::Spinner::new());
                                }
                                _ => {
                                    ui.label(&e.summary);
                                }
                            }
                        });
                        ui.separator();
                        if let Some(r) = &e.repo {
                            ui.hyperlink_to("Source and issues", r);
                        }
                        if let Some(l) = &e.license {
                            ui.label(RichText::new(format!("Licence: {l}")).color(MUTED).small());
                        }
                        let ctxc = compat_context(&self.mods, env.lib);
                        for w in index::compat_issues(&e, &ctxc) {
                            ui.label(RichText::new(format!("⚠ {w}")).color(WARN));
                        }
                        ui.add_space(4.0);
                        install_button(ui, &e, installed.as_ref(), self.op.is_none(), &feed, acts);
                    });
                    if !open {
                        self.details = None;
                    }
                }
            }
        }
        // Versions of an installed mod.
        if let Some(id) = self.versions_for.clone() {
            if !self.releases.contains_key(&id) {
                acts.push(Act::Versions(id.clone()));
            }
            let m = self.mods.iter().find(|m| m.id() == id).cloned();
            let mut open = m.is_some();
            if let Some(m) = m {
                egui::Window::new(format!("{} – versions", m.name())).open(&mut open).default_width(520.0).collapsible(false).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.screen_rect().center()).show(ctx, |ui| {
                    ui.label(RichText::new(format!("Installed: v{}. Any release can be installed, including older ones.", m.version())).color(MUTED));
                    match self.releases.get(&id) {
                        Some(Remote::Ready(f)) => {
                            if let Some(e) = &f.error {
                                ui.label(RichText::new(format!("⚠ {e}")).color(WARN));
                            }
                            if f.releases.is_empty() {
                                ui.label("No installable releases found.");
                            }
                            egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                                for r in &f.releases {
                                    release_row(ui, &m, r, self.op.is_none(), acts);
                                }
                            });
                        }
                        _ => {
                            ui.add(egui::Spinner::new());
                        }
                    }
                });
            }
            if !open {
                self.versions_for = None;
            }
        }
        // Index management.
        if self.show_indexes {
            let mut open = true;
            egui::Window::new("Mod indexes").open(&mut open).default_width(560.0).collapsible(false).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.screen_rect().center()).show(ctx, |ui| {
                ui.label(
                    RichText::new("An index is a GitHub repository listing mods that live in their authors' own repositories. Installing from an index is the same as importing the mod's .zip yourself.")
                        .color(MUTED),
                );
                ui.add_space(4.0);
                for s in &self.sources {
                    egui::Frame::default().fill(CARD_BG).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&s.label).strong());
                            if s.builtin {
                                tag(ui, "BUILT-IN", BLUE);
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if !s.builtin && ui.button("Remove").clicked() {
                                    acts.push(Act::RemoveIndex(s.feed.clone()));
                                }
                            });
                        });
                        ui.label(RichText::new(&s.feed).small().color(MUTED));
                        let status = match self.indexes.get(&s.feed) {
                            Some(Remote::Ready(f)) => match (&f.index, &f.error) {
                                (Some(ix), None) => format!("{} mods · checked {}", ix.mods.len(), text::ago(f.checked_at)),
                                (Some(ix), Some(e)) => format!("{} mods (cached) · {e}", ix.mods.len()),
                                (None, Some(e)) => e.clone(),
                                (None, None) => String::new(),
                            },
                            _ => "loading…".into(),
                        };
                        ui.label(RichText::new(status).small());
                    });
                    ui.add_space(4.0);
                }
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut self.new_index).hint_text("owner/repo or index URL").desired_width(300.0));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Add index").clicked() || enter {
                        acts.push(Act::AddIndex);
                    }
                });
                if let Some(e) = &self.index_error {
                    ui.label(RichText::new(e).color(BAD));
                }
            });
            if !open {
                self.show_indexes = false;
            }
        }
        // Confirmations.
        if let Some(c) = &self.confirm {
            let mut answer: Option<bool> = None;
            let (title, body, yes): (&str, String, &str) = match c {
                Confirm::Uninstall { name, .. } => ("Uninstall mod", format!("Remove {name} from your mods folder? Its saved data and option values are kept."), "Uninstall"),
                Confirm::Replace { info, .. } => {
                    let verb = match dwrc_mods::version::compare(&info.new_version, &info.installed_version) {
                        std::cmp::Ordering::Greater => "Update",
                        std::cmp::Ordering::Less => "Downgrade",
                        std::cmp::Ordering::Equal => "Reinstall",
                    };
                    ("Replace mod", format!("{} v{} is installed. {verb} it to v{} from this file?", info.name, info.installed_version, info.new_version), verb)
                }
                Confirm::IndexInstall { entry, warnings, .. } => (
                    "Before you install",
                    format!("{}:\n\n• {}", entry.title, warnings.join("\n• ")),
                    "Install anyway",
                ),
            };
            egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.set_max_width(440.0);
                ui.label(body);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(yes).clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        answer = Some(false);
                    }
                });
            });
            if let Some(a) = answer {
                let c = self.confirm.take().unwrap();
                if a {
                    match c {
                        Confirm::Uninstall { id, name, dir } => match package::uninstall(&env.paths.user_mods_root(), &dir) {
                            Ok(()) => {
                                env.cfg.mods.forget(&id);
                                self.releases.remove(&id);
                                self.reload(env.paths);
                                if env.running.is_some() {
                                    self.changed_while_running = true;
                                }
                                self.toasts.push((format!("{name} uninstalled."), false));
                                acts.push(Act::Dirty);
                            }
                            Err(e) => self.toasts.push((format!("Could not uninstall {name}: {e:#}"), true)),
                        },
                        Confirm::Replace { zip, .. } => acts.push(Act::Install(Source::File(zip), true)),
                        Confirm::IndexInstall { entry, url, label, .. } => {
                            acts.push(Act::Install(Source::Url { url, id: entry.id.clone(), label }, true));
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Applying actions
    // ------------------------------------------------------------------

    fn apply(&mut self, ctx: &egui::Context, env: &mut Env, acts: Vec<Act>) -> bool {
        let mut changed = false;
        let installed: Vec<String> = self.mods.iter().map(|m| m.id().to_string()).collect();
        let installed_refs: Vec<&str> = installed.iter().map(String::as_str).collect();
        let visible = || -> Vec<String> {
            let order = env.cfg.mods.load_order(installed_refs.iter().copied());
            order
                .into_iter()
                .filter(|id| {
                    let m = self.mods.iter().find(|m| m.id() == id).unwrap();
                    let q = self.search.to_lowercase();
                    q.split_whitespace().all(|t| format!("{} {} {} {}", m.name(), m.id(), m.manifest.info.author, m.manifest.info.description).to_lowercase().contains(t))
                        && self.game_filter.is_none_or(|g| m.supports(g))
                })
                .collect()
        };
        let vis = visible();
        let mut touched_mods = false;
        for a in acts {
            match a {
                Act::Toggle(id, g, on) => {
                    env.cfg.mods.set_enabled(g, &id, on);
                    changed = true;
                    touched_mods = true;
                }
                Act::Move(id, d) => {
                    changed |= env.cfg.mods.move_within(&installed_refs, &vis, &id, d);
                    touched_mods = true;
                }
                Act::Edge(id, top) => {
                    changed |= env.cfg.mods.move_to_edge(&installed_refs, &vis, &id, top);
                    touched_mods = true;
                }
                Act::SetOption(id, opt, v) => {
                    env.cfg.mods.options.entry(id).or_default().insert(opt, v);
                    changed = true;
                    touched_mods = true;
                }
                Act::ResetOptions(id) => {
                    env.cfg.mods.options.remove(&id);
                    touched_mods = true;
                    changed = true;
                }
                Act::Expand(id) => {
                    self.expanded = if self.expanded.as_deref() == Some(id.as_str()) { None } else { Some(id) };
                }
                Act::AskUninstall(id) => {
                    if let Some(m) = self.mods.iter().find(|m| m.id() == id) {
                        self.confirm = Some(Confirm::Uninstall { id: id.clone(), name: m.name().to_string(), dir: m.root.clone() });
                    }
                }
                Act::Install(src, replace) => self.start_install(ctx, env, vec![(src, replace)]),
                Act::UpdateAll => {
                    let jobs: Vec<(Source, bool)> = self
                        .mods
                        .iter()
                        .filter_map(|m| self.offer(m).map(|o| (Source::Url { url: o.url, id: m.id().to_string(), label: format!("Updating {}", m.name()) }, true)))
                        .collect();
                    self.start_install(ctx, env, jobs);
                }
                Act::CheckUpdates => {
                    self.reload(env.paths);
                    self.check_updates(ctx, true);
                    self.fetch_indexes(ctx, true);
                }
                Act::Versions(id) => {
                    if !self.releases.contains_key(&id) {
                        if let Some(m) = self.mods.iter().find(|m| m.id() == id) {
                            if let Some(repo) = m.manifest.info.github_repo() {
                                self.releases.insert(id.clone(), Remote::Loading);
                                let i2 = id.clone();
                                self.spawn(ctx, move |hub| Msg::Releases(i2.clone(), hub.fetch_releases(&repo, &i2, false)));
                            }
                        }
                    }
                    self.versions_for = Some(id);
                }
                Act::EnableAll(on) => {
                    for id in &vis {
                        let m = self.mods.iter().find(|m| m.id() == id).unwrap();
                        if !m.is_usable() {
                            continue;
                        }
                        for g in GameId::ALL {
                            if m.supports(g) && self.game_filter.is_none_or(|f| f == g) {
                                env.cfg.mods.set_enabled(g, id, on);
                            }
                        }
                    }
                    changed = true;
                    touched_mods = true;
                }
                Act::ResetOrder => {
                    env.cfg.mods.order.clear();
                    changed = true;
                    touched_mods = true;
                }
                Act::OpenFolder(p) => {
                    let _ = std::fs::create_dir_all(&p);
                    crate::app::open_folder(&p);
                }
                Act::Details(feed, id) => self.details = Some((feed, id)),
                Act::InstallListing(feed, id) => {
                    let found = self.sources.iter().find(|s| s.feed == feed).and_then(|s| match self.indexes.get(&feed) {
                        Some(Remote::Ready(IndexFetch { index: Some(ix), .. })) => ix.mods.iter().find(|e| e.id == id).map(|e| (s.label.clone(), e.clone())),
                        _ => None,
                    });
                    if let Some((src_label, e)) = found {
                        match e.install_url() {
                            Ok(url) => {
                                let label = format!("Installing {} from {src_label}", e.title);
                                let warnings = index::compat_issues(&e, &compat_context(&self.mods, env.lib));
                                if warnings.is_empty() {
                                    self.start_install(ctx, env, vec![(Source::Url { url, id: e.id.clone(), label }, true)]);
                                } else {
                                    self.confirm = Some(Confirm::IndexInstall { entry: Box::new(e), url, label, warnings });
                                }
                            }
                            Err(why) => self.toasts.push((format!("{} cannot be installed: {why}", e.title), true)),
                        }
                    }
                }
                Act::AddIndex => match index::resolve_source(&self.new_index) {
                    Ok(s) => {
                        if self.sources.iter().any(|x| x.feed == s.feed) {
                            self.index_error = Some("That index is already in the list.".into());
                        } else {
                            env.cfg.mods.indexes.push(s.input.clone());
                            self.new_index.clear();
                            self.index_error = None;
                            self.sources = index::sources(&env.cfg.mods.indexes);
                            self.fetch_indexes(ctx, true);
                            changed = true;
                        }
                    }
                    Err(e) => self.index_error = Some(e),
                },
                Act::RemoveIndex(feed) => {
                    env.cfg.mods.indexes.retain(|i| index::resolve_source(i).map(|s| s.feed != feed).unwrap_or(true));
                    self.indexes.remove(&feed);
                    self.hub.cache.remove("index", &feed);
                    if self.find_source.as_deref() == Some(feed.as_str()) {
                        self.find_source = None;
                    }
                    self.sources = index::sources(&env.cfg.mods.indexes);
                    changed = true;
                }
                Act::RefreshIndexes => self.fetch_indexes(ctx, true),
                Act::Dirty => changed = true,
            }
        }
        if touched_mods && env.running.is_some() {
            self.changed_while_running = true;
        }
        changed
    }

    // ------------------------------------------------------------------
    // In-game (F10) panel: the running game's mods only.
    // ------------------------------------------------------------------

    pub fn game_panel(&mut self, ui: &mut egui::Ui, env: &mut Env, g: GameId) -> bool {
        self.ensure_loaded(env);
        let mut acts = Vec::new();
        let order = env.cfg.mods.load_order(self.mods.iter().map(|m| m.id()));
        let skips: HashMap<String, Skip> =
            dwrc_runtime::mod_plan(env.cfg, &self.mods, g, None).1.into_iter().map(|(m, s)| (m.id().to_string(), s)).collect();
        if env.cfg.mods.safe_mode {
            ui.label(RichText::new("Safe mode is on: no mods load.").color(WARN));
        }
        let list: Vec<&ModInfo> = order.iter().filter_map(|id| self.mods.iter().find(|m| m.id() == id.as_str())).filter(|m| m.supports(g)).collect();
        if list.is_empty() {
            ui.label("No installed mods run on this game. Install mods from the Mods page.");
        }
        for m in list {
            let id = m.id().to_string();
            let mut on = env.cfg.mods.is_enabled(g, &id);
            ui.horizontal(|ui| {
                if ui.add_enabled(m.is_usable(), egui::Checkbox::new(&mut on, "")).changed() {
                    acts.push(Act::Toggle(id.clone(), g, on));
                }
                let sel = self.expanded.as_deref() == Some(id.as_str());
                if ui.selectable_label(sel, RichText::new(format!("{}  v{}", m.name(), m.version())).strong()).clicked() {
                    acts.push(Act::Expand(id.clone()));
                }
            });
            if let Some(why) = skips.get(&id) {
                ui.label(RichText::new(format!("   ⚠ not loading: {why}")).color(WARN).small());
            }
            if self.expanded.as_deref() == Some(id.as_str()) && !m.manifest.options.is_empty() {
                egui::Frame::default().fill(Color32::from_rgb(24, 27, 33)).inner_margin(8.0).corner_radius(6.0).show(ui, |ui| {
                    options_editor(ui, &id, &m.manifest.options, env.cfg.mods.options.get(&id), &mut acts);
                });
            }
        }
        let ctx = ui.ctx().clone();
        self.apply(&ctx, env, acts)
    }
}

fn compat_context(mods: &[ModInfo], lib: &Library) -> CompatContext {
    CompatContext {
        installed: mods.iter().map(|m| (m.id().to_string(), m.version().to_string())).collect(),
        imported: lib.games.keys().copied().collect(),
    }
}

fn tag(ui: &mut egui::Ui, s: &str, col: Color32) {
    egui::Frame::default().stroke(Stroke::new(1.0_f32, col)).corner_radius(4.0).inner_margin(egui::Margin::symmetric(5, 1)).show(ui, |ui| {
        ui.label(RichText::new(s).small().color(col));
    });
}

fn banner(ui: &mut egui::Ui, col: Color32, msg: &str, extra: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::default().fill(col.gamma_multiply(0.18)).stroke(Stroke::new(1.0_f32, col)).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(msg).color(Color32::WHITE));
            extra(ui);
        });
    });
}

fn options_editor(ui: &mut egui::Ui, id: &str, defs: &[dwrc_mods::OptionDef], saved: Option<&std::collections::BTreeMap<String, toml::Value>>, acts: &mut Vec<Act>) {
    egui::Grid::new(format!("opts-{id}")).num_columns(3).spacing([10.0, 5.0]).show(ui, |ui| {
        for o in defs {
            let cur = saved.and_then(|s| s.get(&o.id)).cloned().unwrap_or_else(|| o.default.clone());
            ui.label(&o.name);
            let set = |acts: &mut Vec<Act>, v: toml::Value| acts.push(Act::SetOption(id.to_string(), o.id.clone(), v));
            match o.kind {
                OptionKind::Bool => {
                    let mut v = cur.as_bool().unwrap_or(false);
                    if ui.checkbox(&mut v, "").changed() {
                        set(acts, toml::Value::Boolean(v));
                    }
                }
                OptionKind::Int => {
                    let mut v = cur.as_integer().unwrap_or(0);
                    let lo = o.min.unwrap_or(i32::MIN as f64) as i64;
                    let hi = o.max.unwrap_or(i32::MAX as f64) as i64;
                    if ui.add(egui::DragValue::new(&mut v).range(lo..=hi)).changed() {
                        set(acts, toml::Value::Integer(v));
                    }
                }
                OptionKind::Float => {
                    let mut v = cur.as_float().or_else(|| cur.as_integer().map(|i| i as f64)).unwrap_or(0.0);
                    let lo = o.min.unwrap_or(f64::MIN);
                    let hi = o.max.unwrap_or(f64::MAX);
                    if ui.add(egui::DragValue::new(&mut v).range(lo..=hi).speed(0.05)).changed() {
                        set(acts, toml::Value::Float(v));
                    }
                }
                OptionKind::Choice => {
                    let mut v = cur.as_str().unwrap_or("").to_string();
                    let before = v.clone();
                    egui::ComboBox::from_id_salt(format!("{id}-{}", o.id)).selected_text(&v).show_ui(ui, |ui| {
                        for c in &o.choices {
                            ui.selectable_value(&mut v, c.clone(), c);
                        }
                    });
                    if v != before {
                        set(acts, toml::Value::String(v));
                    }
                }
            }
            ui.label(RichText::new(&o.description).color(MUTED).small());
            ui.end_row();
        }
    });
}

fn install_button(ui: &mut egui::Ui, e: &IndexEntry, installed: Option<&ModInfo>, idle: bool, feed: &str, acts: &mut Vec<Act>) {
    let listed = e.display_version();
    match (installed, e.install_url()) {
        (Some(m), _) if !dwrc_mods::version::is_newer(&listed, m.version()) => {
            ui.add_enabled(false, egui::Button::new(format!("✔ Installed v{}", m.version())));
        }
        (Some(m), Ok(_)) => {
            let r = ui.add_enabled_ui(idle, |ui| crate::footer::update_button(ui, &format!("Update to v{listed}"))).inner;
            if r.on_hover_text(format!("You have v{}", m.version())).clicked() {
                acts.push(Act::InstallListing(feed.to_string(), e.id.clone()));
            }
        }
        (None, Ok(_)) => {
            let b = egui::Button::new(RichText::new("Install").strong().color(Color32::BLACK)).fill(ACCENT).min_size(Vec2::new(90.0, 26.0));
            if ui.add_enabled(idle, b).clicked() {
                acts.push(Act::InstallListing(feed.to_string(), e.id.clone()));
            }
        }
        (_, Err(why)) => {
            ui.add_enabled(false, egui::Button::new("Not installable")).on_disabled_hover_text(why);
        }
    }
}

fn listing_card(ui: &mut egui::Ui, src: &IndexSource, e: &IndexEntry, tex: Option<&TextureHandle>, installed: Option<&ModInfo>, idle: bool, acts: &mut Vec<Act>) {
    egui::Frame::default().fill(CARD_BG).corner_radius(8.0).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let size = Vec2::new(112.0, 84.0);
            match tex {
                Some(t) => {
                    let ts = t.size_vec2();
                    let k = (size.x / ts.x).min(size.y / ts.y);
                    ui.add(egui::Image::new(egui::load::SizedTexture::new(t.id(), ts * k)));
                }
                None => {
                    let (r, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    ui.painter().rect_filled(r, 6.0, Color32::from_rgb(22, 26, 34));
                    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, e.title.chars().next().unwrap_or('?').to_string(), egui::FontId::proportional(34.0), Color32::from_rgb(55, 62, 78));
                }
            }
            ui.vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.add(egui::Label::new(RichText::new(&e.title).size(16.0).strong().color(Color32::WHITE)).sense(egui::Sense::click())).on_hover_text("Details").clicked() {
                        acts.push(Act::Details(src.feed.clone(), e.id.clone()));
                    }
                    let v = e.display_version();
                    if v != "?" {
                        ui.label(RichText::new(format!("v{v}")).color(MUTED));
                    }
                    if let Some(a) = &e.author {
                        ui.label(RichText::new(format!("by {a}")).color(MUTED));
                    }
                    if e.experimental {
                        tag(ui, "EXPERIMENTAL", WARN);
                    }
                });
                if !e.summary.is_empty() {
                    ui.label(text::preview_line(&e.summary, 180));
                }
                ui.horizontal_wrapped(|ui| {
                    for g in e.game_ids() {
                        tag(ui, game_short(g), game_color(g));
                    }
                    for c in e.categories.iter().take(3) {
                        tag(ui, &c.to_uppercase(), MUTED);
                    }
                    let mut stats = Vec::new();
                    if let Some(d) = e.total_downloads() {
                        stats.push(format!("{} downloads", text::short_count(d)));
                    }
                    if let Some(u) = e.updated() {
                        stats.push(format!("updated {u}"));
                    }
                    if !stats.is_empty() {
                        ui.label(RichText::new(stats.join(" · ")).color(MUTED).small());
                    }
                });
                ui.horizontal(|ui| {
                    install_button(ui, e, installed, idle, &src.feed, acts);
                    if ui.button("Details").clicked() {
                        acts.push(Act::Details(src.feed.clone(), e.id.clone()));
                    }
                });
            });
        });
    });
    ui.add_space(6.0);
}

fn release_row(ui: &mut egui::Ui, m: &ModInfo, r: &Release, idle: bool, acts: &mut Vec<Act>) {
    egui::Frame::default().fill(CARD_BG).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("v{}", r.version)).strong());
            if r.name != r.version && !r.name.trim_start_matches(['v', 'V']).eq(&r.version) {
                ui.label(&r.name);
            }
            if r.prerelease {
                tag(ui, "PRE-RELEASE", WARN);
            }
            if let Some(p) = &r.published {
                ui.label(RichText::new(p).color(MUTED).small());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let cmp = dwrc_mods::version::compare(&r.version, m.version());
                let label = match cmp {
                    std::cmp::Ordering::Equal => "Reinstall",
                    std::cmp::Ordering::Greater => "Install",
                    std::cmp::Ordering::Less => "Roll back",
                };
                if ui.add_enabled(idle, egui::Button::new(label)).clicked() {
                    acts.push(Act::Install(Source::Url { url: r.zip.url.clone(), id: m.id().to_string(), label: format!("Installing {} v{}", m.name(), r.version) }, true));
                }
                if cmp == std::cmp::Ordering::Equal {
                    ui.label(RichText::new("installed").color(OK).small());
                }
            });
        });
        if !r.body.trim().is_empty() {
            egui::CollapsingHeader::new("Patch notes").id_salt(format!("notes-{}", r.tag)).show(ui, |ui| {
                ui.label(text::clean_markdown(&r.body, 3000));
            });
        }
    });
    ui.add_space(4.0);
}

/// PNG bytes to RGBA (thumbnails are PNG, at most 1024×1024).
fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut r = dec.read_info().map_err(|e| e.to_string())?;
    let (w, h) = (r.info().width, r.info().height);
    if w > 1024 || h > 1024 {
        return Err("thumbnail too large".into());
    }
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let px = (info.width * info.height) as usize;
    let rgba = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => buf[..px * 4].to_vec(),
        (png::ColorType::Rgb, png::BitDepth::Eight) => buf[..px * 3].chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight) => buf[..px * 2].chunks(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        (png::ColorType::Grayscale, png::BitDepth::Eight) => buf[..px].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        _ => return Err("unsupported PNG format (use 8-bit RGB or RGBA)".into()),
    };
    Ok((info.width, info.height, rgba))
}
