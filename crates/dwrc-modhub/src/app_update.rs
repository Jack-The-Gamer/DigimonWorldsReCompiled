//! Updating Re:Compiled itself.
//!
//! **Checking.** The launcher reads the project's latest GitHub release
//! (`GET /repos/<repo>/releases/latest`, which never returns drafts or
//! pre-releases) and offers it when its version is newer than the running
//! build. It asks on every start and every six hours while it stays open;
//! the last answer is kept on disk for when GitHub cannot be reached.
//!
//! **Installing** (a copy unzipped from a release): the zip CI built for this
//! platform is downloaded and unpacked into a staging folder beside the
//! program, then each top-level item it contains is swapped in: the old item
//! is renamed to `<name>.dwrc-old` and the new one renamed into place. A
//! failure part-way puts every old item back. Renaming (rather than
//! overwriting) is what lets a running program replace itself on Windows,
//! and the running copy keeps working until it is restarted. Only names the
//! release contains are touched, so `portable.txt`, a portable `data/`
//! folder or anything else a player keeps there is left alone; folders the
//! program owns (`mods/` with the bundled template, `docs/`) are replaced
//! whole, so files a release dropped disappear. Leftover `*.dwrc-old`
//! items are removed by [`cleanup`] on the next start.
//!
//! **Source builds** (an executable in a checkout's `target/` folder) are
//! not replaced: they update by pulling the source and rebuilding.

use crate::github::Asset;
use crate::{cache, Hub};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

/// Where Re:Compiled is released.
pub const APP_REPO: &str = "Jack-The-Gamer/DigimonWorldsReCompiled";
/// How long a checked latest release is trusted (and how often a launcher
/// left open asks again).
pub const APP_TTL: u64 = 6 * 60 * 60;
/// The running version.
pub const RUNNING: &str = env!("CARGO_PKG_VERSION");

/// The program's file name in a release.
pub const EXE_NAME: &str = if cfg!(windows) { "DigimonWorldsReCompiled.exe" } else { "DigimonWorldsReCompiled" };
/// Suffix of items moved aside by an update.
const OLD: &str = ".dwrc-old";
/// Staging folder, next to the program.
const STAGE: &str = ".dwrc-update";

/// The zip CI builds for this platform, if it builds one.
pub fn platform_asset() -> Option<&'static str> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("DigimonWorldsReCompiled-windows-x64.zip")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("DigimonWorldsReCompiled-linux-x64.zip")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("DigimonWorldsReCompiled-macos-arm64.zip")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("DigimonWorldsReCompiled-macos-x64.zip")
    } else {
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppRelease {
    /// Normalised (`0.2.2`).
    pub version: String,
    pub tag: String,
    pub name: String,
    /// Release notes (markdown).
    pub notes: String,
    /// The release's page on GitHub.
    pub page: String,
    /// `YYYY-MM-DD`.
    pub published: Option<String>,
    /// This platform's zip, when the release has one.
    pub zip: Option<Asset>,
}

impl AppRelease {
    /// Whether this release is newer than `running`.
    pub fn is_newer_than(&self, running: &str) -> bool {
        dwrc_mods::version::is_newer(&self.version, running)
    }
}

/// Decodes `GET /repos/{repo}/releases/latest`, picking `asset` as the zip.
pub fn parse_latest(json: &str, asset: Option<&str>) -> Result<AppRelease> {
    let r: Value = serde_json::from_str(json).map_err(|e| anyhow!("GitHub sent something unexpected ({e})"))?;
    let s = |k: &str| r.get(k).and_then(Value::as_str).map(str::to_string);
    let tag = s("tag_name").ok_or_else(|| anyhow!("GitHub: {}", s("message").unwrap_or_else(|| "no release found".into())))?;
    let version = dwrc_mods::version::parse(&tag).ok_or_else(|| anyhow!("the latest release's tag '{tag}' is not a version"))?.to_string();
    let zip = asset.and_then(|want| {
        r.get("assets")?.as_array()?.iter().find_map(|a| {
            let name = a.get("name")?.as_str()?;
            if !name.eq_ignore_ascii_case(want) {
                return None;
            }
            Some(Asset {
                name: name.to_string(),
                url: a.get("browser_download_url")?.as_str()?.to_string(),
                size: a.get("size").and_then(Value::as_u64),
            })
        })
    });
    Ok(AppRelease {
        name: s("name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| tag.clone()),
        notes: s("body").unwrap_or_default(),
        page: s("html_url").unwrap_or_else(|| format!("https://github.com/{APP_REPO}/releases")),
        published: s("published_at").map(|d| d.chars().take(10).collect()),
        version,
        tag,
        zip,
    })
}

/// The outcome of a check.
#[derive(Debug, Clone)]
pub struct AppCheck {
    pub release: Option<AppRelease>,
    /// Set when the live check failed (a cached answer may still be given).
    pub error: Option<String>,
    pub checked_at: u64,
}

impl AppCheck {
    /// The release to offer, if it is newer than the running build.
    pub fn update(&self) -> Option<&AppRelease> {
        self.release.as_ref().filter(|r| r.is_newer_than(RUNNING))
    }
}

impl Hub {
    /// Reads Re:Compiled's latest release: the cached answer while it is
    /// fresh (unless `force`), else GitHub, falling back to a stale cache.
    pub fn check_app(&self, force: bool) -> AppCheck {
        let cached = self.cache.get::<AppRelease>("app", APP_REPO);
        if let Some(c) = &cached {
            if !force && c.age() < APP_TTL {
                return AppCheck { release: Some(c.data.clone()), error: None, checked_at: c.checked_at };
            }
        }
        let live = self.http.github(&format!("/repos/{APP_REPO}/releases/latest")).and_then(|j| parse_latest(&j, platform_asset()));
        match live {
            Ok(r) => {
                self.cache.put("app", APP_REPO, &r);
                AppCheck { release: Some(r), error: None, checked_at: cache::now() }
            }
            Err(e) => AppCheck {
                release: cached.as_ref().map(|c| c.data.clone()),
                error: Some(format!("{e:#}")),
                checked_at: cached.map(|c| c.checked_at).unwrap_or(0),
            },
        }
    }
}

/// How the running copy was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// Unzipped from a release into `dir`: it can update itself.
    Release { dir: PathBuf },
    /// Built in a source checkout at `repo`: it updates by pulling and
    /// rebuilding.
    Source { repo: PathBuf },
}

impl Install {
    pub fn detect() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        // macOS may report the path it was started by (a symlink); Windows'
        // canonical form is a \\?\ path, which is better avoided.
        let exe = if cfg!(windows) { exe } else { exe.canonicalize().unwrap_or(exe) };
        Some(Self::for_dir(exe.parent()?))
    }

    pub fn for_dir(dir: &Path) -> Self {
        let profile = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if matches!(profile, "debug" | "release") {
            if let Some(target) = dir.parent().filter(|t| t.file_name().is_some_and(|n| n == "target")) {
                if let Some(repo) = target.parent().filter(|r| r.join("Cargo.toml").is_file()) {
                    return Self::Source { repo: repo.to_path_buf() };
                }
            }
        }
        Self::Release { dir: dir.to_path_buf() }
    }
}

/// Unpacks the release zip at `zip_path` and swaps its contents into
/// `dir`. Returns the new program's path.
pub fn apply(zip_path: &Path, dir: &Path) -> Result<PathBuf> {
    apply_inner(zip_path, dir, None)
}

fn apply_inner(zip_path: &Path, dir: &Path, fail_on: Option<&str>) -> Result<PathBuf> {
    let stage = dir.join(STAGE);
    if stage.exists() {
        std::fs::remove_dir_all(&stage).with_context(|| format!("could not clear {}", stage.display()))?;
    }
    std::fs::create_dir_all(&stage).with_context(|| format!("could not write in {} (is the folder read-only?)", dir.display()))?;
    let result = (|| {
        extract_release(zip_path, &stage)?;
        if !stage.join(EXE_NAME).is_file() {
            bail!("the download does not contain {EXE_NAME}; nothing was changed");
        }
        swap_in(&stage, dir, fail_on)?;
        Ok(dir.join(EXE_NAME))
    })();
    let _ = std::fs::remove_dir_all(&stage);
    result
}

/// Extracts a release zip into `dest`, dropping the single top-level folder
/// CI wraps it in (`DigimonWorldsReCompiled-<platform>/`).
fn extract_release(zip_path: &Path, dest: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(File::open(zip_path)?).context("the download is not a valid .zip")?;
    let mut items: Vec<(usize, PathBuf, bool, u64, Option<u32>)> = Vec::new();
    for i in 0..zip.len() {
        let f = zip.by_index(i)?;
        let path = f.enclosed_name().ok_or_else(|| anyhow!("the download contains an unsafe path: {}", f.name()))?;
        if f.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            bail!("the download contains a symbolic link: {}", f.name());
        }
        items.push((i, path, f.is_dir(), f.size(), f.unix_mode()));
    }
    let first = |p: &Path| p.components().next().map(|c| c.as_os_str().to_os_string());
    let top: Option<OsString> = items.first().and_then(|(_, p, ..)| first(p));
    let wrapped = top.is_some()
        && items.iter().all(|(_, p, dir, ..)| first(p) == top && (*dir || p.components().count() > 1));
    for (i, path, dir, size, mode) in items {
        let rel = if wrapped { path.iter().skip(1).collect::<PathBuf>() } else { path.clone() };
        if rel.as_os_str().is_empty() {
            continue;
        }
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            bail!("the download contains an unsafe path: {}", path.display());
        }
        let out = dest.join(&rel);
        if dir {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut f = zip.by_index(i)?;
        let mut w = std::io::BufWriter::new(File::create(&out).with_context(|| format!("writing {}", out.display()))?);
        let n = std::io::copy(&mut (&mut f).take(size + 1), &mut w).with_context(|| format!("extracting {}", path.display()))?;
        if n != size {
            bail!("{} is damaged in the download", path.display());
        }
        w.flush()?;
        drop(w);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut m = mode.map(|m| m & 0o777).filter(|m| *m != 0).unwrap_or(0o644);
            if rel == Path::new(EXE_NAME) || rel.extension().is_some_and(|e| e == "sh" || e == "command") || rel == Path::new("dwrc-cli") {
                m |= 0o755;
            }
            std::fs::set_permissions(&out, std::fs::Permissions::from_mode(m))?;
        }
        #[cfg(not(unix))]
        let _ = mode;
    }
    Ok(())
}

/// A free name to move `name` aside to: `<name>.dwrc-old`, or
/// `<name>.dwrc-old-<n>` while an older one cannot be removed (Windows keeps
/// a program that is still running).
fn backup_name(dir: &Path, name: &OsString) -> PathBuf {
    let base = format!("{}{OLD}", name.to_string_lossy());
    let first = dir.join(&base);
    if remove_any(&first).is_ok() {
        return first;
    }
    (1..)
        .map(|n| dir.join(format!("{base}-{n}")))
        .find(|p| remove_any(p).is_ok())
        .expect("an unused name")
}

/// Removes a file or folder; Ok when it no longer exists.
fn remove_any(p: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
        Ok(_) => std::fs::remove_file(p),
    }
}

fn swap_in(stage: &Path, dir: &Path, fail_on: Option<&str>) -> Result<()> {
    let mut names: Vec<OsString> = std::fs::read_dir(stage)?.flatten().map(|e| e.file_name()).collect();
    names.sort();
    // (target, where the old one went)
    let mut done: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let rollback = |done: &[(PathBuf, Option<PathBuf>)]| {
        for (target, old) in done.iter().rev() {
            let _ = remove_any(target);
            if let Some(old) = old {
                let _ = std::fs::rename(old, target);
            }
        }
    };
    for name in names {
        let target = dir.join(&name);
        let old = match std::fs::symlink_metadata(&target) {
            Ok(_) => {
                let b = backup_name(dir, &name);
                if let Err(e) = std::fs::rename(&target, &b) {
                    rollback(&done);
                    bail!("could not replace {} ({e}); nothing was changed. Close anything using the program's folder and try again", target.display());
                }
                Some(b)
            }
            Err(_) => None,
        };
        let moved = if fail_on.is_some_and(|f| name == f) {
            Err(std::io::Error::other("simulated failure"))
        } else {
            std::fs::rename(stage.join(&name), &target)
        };
        if let Err(e) = moved {
            if let Some(b) = &old {
                let _ = std::fs::rename(b, &target);
            }
            rollback(&done);
            bail!("could not install {} ({e}); nothing was changed", target.display());
        }
        done.push((target, old));
    }
    // Best effort now; whatever is still in use goes on the next start.
    for (_, old) in &done {
        if let Some(old) = old {
            let _ = remove_any(old);
        }
    }
    Ok(())
}

/// Removes what earlier updates moved aside in `dir`. Returns how many
/// items are left (still in use).
pub fn cleanup(dir: &Path) -> usize {
    let _ = remove_any(&dir.join(STAGE));
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut left = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let ours = name.ends_with(OLD) || name.rsplit_once(&format!("{OLD}-")).is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if ours && remove_any(&e.path()).is_err() {
            left += 1;
        }
    }
    left
}

/// Starts `exe` (the updated program) as a new process.
pub fn relaunch(exe: &Path) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(exe);
    if let Some(d) = exe.parent() {
        cmd.current_dir(d);
    }
    cmd.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dwrc-appupdate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn release_zip(path: &Path, top: &str, files: &[(&str, &str)]) {
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        let o = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
        w.add_directory(format!("{top}/"), o).unwrap();
        for (name, body) in files {
            w.start_file(format!("{top}/{name}"), o).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap_or_else(|_| "<missing>".into())
    }

    fn old_install(dir: &Path) {
        std::fs::write(dir.join(EXE_NAME), "old exe").unwrap();
        std::fs::write(dir.join("README.md"), "old readme").unwrap();
        std::fs::create_dir_all(dir.join("mods/play-time")).unwrap();
        std::fs::write(dir.join("mods/play-time/mod.toml"), "old mod").unwrap();
        std::fs::write(dir.join("portable.txt"), "").unwrap();
        std::fs::create_dir_all(dir.join("data/saves")).unwrap();
        std::fs::write(dir.join("data/saves/card1.mcd"), "save").unwrap();
    }

    const NEW: &[(&str, &str)] = &[
        (EXE_NAME, "new exe"),
        ("README.md", "new readme"),
        ("mods/example-template/mod.toml", "template"),
        ("docs/modding.md", "docs"),
    ];

    #[test]
    fn parses_latest() {
        let json = r#"{"tag_name":"v0.3.0","name":"","body":"* new","html_url":"https://github.com/x/y/releases/tag/v0.3.0","published_at":"2026-11-01T10:00:00Z",
          "assets":[{"name":"DigimonWorldsReCompiled-linux-x64.zip","browser_download_url":"https://x/l.zip","size":5},
                    {"name":"DigimonWorldsReCompiled-windows-x64.zip","browser_download_url":"https://x/w.zip","size":6}]}"#;
        let r = parse_latest(json, Some("DigimonWorldsReCompiled-windows-x64.zip")).unwrap();
        assert_eq!((r.version.as_str(), r.name.as_str(), r.published.as_deref()), ("0.3.0", "v0.3.0", Some("2026-11-01")));
        assert_eq!(r.zip.unwrap().url, "https://x/w.zip");
        assert!(parse_latest(json, Some("nope.zip")).unwrap().zip.is_none());
        assert!(r.page.ends_with("v0.3.0"));
        let mut r = parse_latest(json, None).unwrap();
        assert!(r.is_newer_than("0.2.1") && !r.is_newer_than("0.3.0"));
        r.version = "0.2.1".into();
        assert!(!r.is_newer_than("0.2.1"));
        assert!(parse_latest(r#"{"message":"Not Found"}"#, None).unwrap_err().to_string().contains("Not Found"));
    }

    #[test]
    fn swaps_files_and_keeps_player_data() {
        let d = tmp("swap");
        let dir = d.join("app");
        std::fs::create_dir_all(&dir).unwrap();
        old_install(&dir);
        let z = d.join("rel.zip");
        release_zip(&z, "DigimonWorldsReCompiled-linux-x64", NEW);
        let exe = apply(&z, &dir).unwrap();
        assert_eq!(exe, dir.join(EXE_NAME));
        assert_eq!(read(&exe), "new exe");
        assert_eq!(read(&dir.join("README.md")), "new readme");
        assert_eq!(read(&dir.join("mods/example-template/mod.toml")), "template");
        assert!(!dir.join("mods/play-time").exists(), "the bundled mods folder is replaced whole");
        assert_eq!(read(&dir.join("data/saves/card1.mcd")), "save", "player data untouched");
        assert!(dir.join("portable.txt").exists());
        assert!(!dir.join(STAGE).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&exe).unwrap().permissions().mode() & 0o111, 0o111, "the program is executable");
        }
        // Nothing left behind once nothing is in use.
        cleanup(&dir);
        let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert!(names.iter().all(|n| !n.contains(OLD)), "{names:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn failure_puts_everything_back() {
        let d = tmp("fail");
        let dir = d.join("app");
        std::fs::create_dir_all(&dir).unwrap();
        old_install(&dir);
        let z = d.join("rel.zip");
        release_zip(&z, "DigimonWorldsReCompiled-linux-x64", NEW);
        // The program is swapped in (names go in order) before README.md fails.
        let e = apply_inner(&z, &dir, Some("README.md")).unwrap_err();
        assert!(e.to_string().contains("nothing was changed"), "{e}");
        assert_eq!(read(&dir.join(EXE_NAME)), "old exe");
        assert_eq!(read(&dir.join("README.md")), "old readme");
        assert_eq!(read(&dir.join("mods/play-time/mod.toml")), "old mod");
        assert!(!dir.join("docs").exists());
        assert!(!dir.join(STAGE).exists());
        // A download without the program changes nothing either.
        release_zip(&z, "x", &[("README.md", "?")]);
        assert!(apply(&z, &dir).unwrap_err().to_string().contains("does not contain"));
        assert_eq!(read(&dir.join("README.md")), "old readme");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn leftovers_and_names() {
        let d = tmp("left");
        std::fs::write(d.join(format!("{EXE_NAME}{OLD}")), "").unwrap();
        std::fs::create_dir_all(d.join(format!("docs{OLD}-2"))).unwrap();
        std::fs::write(d.join("notes.dwrc-old-x"), "").unwrap();
        std::fs::write(d.join("keep.txt"), "").unwrap();
        assert_eq!(cleanup(&d), 0);
        let mut names: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        names.sort();
        assert_eq!(names, vec!["keep.txt", "notes.dwrc-old-x"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn source_builds() {
        let d = tmp("src");
        std::fs::write(d.join("Cargo.toml"), "").unwrap();
        std::fs::create_dir_all(d.join("target/release")).unwrap();
        assert_eq!(Install::for_dir(&d.join("target/release")), Install::Source { repo: d.clone() });
        assert_eq!(Install::for_dir(&d), Install::Release { dir: d.clone() });
        let _ = std::fs::remove_dir_all(&d);
    }
}
