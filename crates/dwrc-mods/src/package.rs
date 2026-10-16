//! Mod packages: installing a mod from a `.zip`, removing an installed mod,
//! and packing a mod folder into the release `.zip` players install.
//!
//! The archive is checked before anything is copied: it must hold exactly
//! one mod (a `mod.toml` at its root, or inside a single folder — a GitHub
//! "Download ZIP" of a repository whose root is the mod works too), every
//! path must stay inside the install folder, and sizes are capped. The mod
//! is extracted to a staging folder, validated there, and only then swapped
//! into `mods/<id>/`, replacing any older copy of the same id (wherever it
//! was installed). A failure at any step leaves the previous install as it
//! was.

use crate::manifest::{valid_id, ModManifest};
use anyhow::{anyhow, bail, Context, Result};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

/// Most entries an archive may hold.
pub const MAX_ENTRIES: usize = 20_000;
/// Largest total uncompressed size.
pub const MAX_TOTAL: u64 = 1 << 30;
/// Largest single file.
pub const MAX_FILE: u64 = 512 << 20;
/// How deep the mod folder may sit inside the archive.
const MAX_DEPTH: usize = 3;

/// What an archive contains, read without extracting it.
#[derive(Debug, Clone)]
pub struct ZipMod {
    pub manifest: ModManifest,
    /// Folder inside the archive holding `mod.toml` (`""` for the root).
    pub prefix: String,
    pub files: usize,
    pub size: u64,
}

/// A same-id mod is already installed (ask before replacing it).
#[derive(Debug, Clone)]
pub struct AlreadyInstalled {
    pub id: String,
    pub name: String,
    pub installed_version: String,
    pub new_version: String,
}

impl std::fmt::Display for AlreadyInstalled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} v{} is already installed", self.name, self.installed_version)
    }
}

impl std::error::Error for AlreadyInstalled {}

/// The result of an install.
#[derive(Debug, Clone)]
pub struct Installed {
    pub id: String,
    pub name: String,
    pub version: String,
    pub dir: PathBuf,
    /// The version that was replaced, if any.
    pub replaced: Option<String>,
    pub manifest: ModManifest,
}

#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// Refuse an archive whose mod id differs (installs from an index or a
    /// mod's own releases must deliver the mod they were listed as).
    pub expect_id: Option<String>,
    /// Replace an installed mod with the same id (updates, other versions).
    pub replace: bool,
}

fn ignorable(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s == "__MACOSX" || s == ".DS_Store" || s == "Thumbs.db" || s == "desktop.ini" || s == ".git"
    })
}

fn is_symlink(mode: Option<u32>) -> bool {
    mode.is_some_and(|m| m & 0o170000 == 0o120000)
}

/// Lists an archive's entries as safe relative paths, rejecting anything
/// that could escape the folder it is extracted into.
fn entries(zip: &mut zip::ZipArchive<File>) -> Result<Vec<(usize, PathBuf, bool, u64)>> {
    if zip.len() > MAX_ENTRIES {
        bail!("the .zip has {} entries (at most {MAX_ENTRIES} are allowed)", zip.len());
    }
    let mut out = Vec::with_capacity(zip.len());
    let mut total = 0u64;
    for i in 0..zip.len() {
        let f = zip.by_index_raw(i)?;
        let name = f.name().to_string();
        let path = f.enclosed_name().ok_or_else(|| anyhow!("the .zip contains an unsafe path: {name}"))?;
        if is_symlink(f.unix_mode()) {
            bail!("the .zip contains a symbolic link ({name}); mods may only contain files");
        }
        if ignorable(&path) {
            continue;
        }
        if f.size() > MAX_FILE {
            bail!("{name} is too large ({} MB)", f.size() >> 20);
        }
        total += f.size();
        if total > MAX_TOTAL {
            bail!("the .zip unpacks to more than {} MB", MAX_TOTAL >> 20);
        }
        out.push((i, path, f.is_dir(), f.size()));
    }
    Ok(out)
}

/// Reads an archive and finds the mod inside it.
pub fn inspect_zip(path: &Path) -> Result<ZipMod> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| anyhow!("not a valid .zip file ({e})"))?;
    let list = entries(&mut zip)?;
    // The shallowest mod.toml names the mod folder.
    let mut found: Vec<&PathBuf> =
        list.iter().filter(|(_, p, dir, _)| !dir && p.file_name().is_some_and(|n| n == "mod.toml")).map(|(_, p, _, _)| p).collect();
    if found.is_empty() {
        bail!("no mod.toml found in the .zip — is this a Re:Compiled mod?");
    }
    let depth = |p: &Path| p.components().count();
    found.sort_by_key(|p| depth(p));
    let top = depth(found[0]);
    if found.iter().filter(|p| depth(p) == top).count() > 1 {
        bail!("the .zip contains more than one mod; install them one at a time");
    }
    if top > MAX_DEPTH + 1 {
        bail!("mod.toml is nested too deeply in the .zip");
    }
    let manifest_path = found[0].clone();
    let prefix_path = manifest_path.parent().map(Path::to_path_buf).unwrap_or_default();
    let prefix = prefix_path.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect::<Vec<_>>().join("/");
    let idx = list.iter().find(|(_, p, _, _)| *p == manifest_path).map(|e| e.0).unwrap();
    let mut text = String::new();
    zip.by_index(idx)?.take(1 << 20).read_to_string(&mut text).context("reading mod.toml")?;
    let manifest = ModManifest::parse(&text).map_err(|e| anyhow!("mod.toml in the .zip is not valid: {e:#}"))?;
    let (mut files, mut size) = (0, 0);
    for (_, p, dir, s) in &list {
        if !dir && p.starts_with(&prefix_path) {
            files += 1;
            size += s;
        }
    }
    Ok(ZipMod { manifest, prefix, files, size })
}

fn unique_suffix() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{}-{n}", std::process::id())
}

/// Every installed copy of `id` under `mods_root` (current and older
/// layouts), plus a manifest-less `mods_root/<id>` left by an interrupted
/// install, so an update never leaves an older copy that still wins.
fn same_id_dirs(mods_root: &Path, id: &str) -> Vec<(PathBuf, Option<ModManifest>)> {
    let mut out: Vec<(PathBuf, Option<ModManifest>)> = Vec::new();
    for dir in crate::mod_folders(mods_root) {
        if let Ok(m) = ModManifest::load(&dir.join("mod.toml")) {
            if m.info.id == id {
                out.push((dir, Some(m)));
            }
        }
    }
    let direct = mods_root.join(id);
    if direct.exists() && !out.iter().any(|(p, _)| *p == direct) {
        out.push((direct.clone(), ModManifest::load(&direct.join("mod.toml")).ok()));
    }
    out
}

fn extract(zip_path: &Path, prefix: &str, dest: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(File::open(zip_path)?)?;
    let prefix = PathBuf::from(prefix);
    for (i, path, dir, size) in entries(&mut zip)? {
        let Ok(rel) = path.strip_prefix(&prefix) else { continue };
        if rel.as_os_str().is_empty() {
            continue;
        }
        // enclosed_name() already rejects absolute paths and '..'; check again
        // after stripping the prefix for defence in depth.
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            bail!("unsafe path in archive: {}", path.display());
        }
        let out = dest.join(rel);
        if dir {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = zip.by_index(i)?;
        let mut w = std::io::BufWriter::new(File::create(&out).with_context(|| format!("writing {}", out.display()))?);
        let n = std::io::copy(&mut (&mut f).take(size + 1), &mut w).with_context(|| format!("extracting {}", path.display()))?;
        if n != size {
            bail!("{} is damaged in the .zip", path.display());
        }
        w.flush()?;
    }
    Ok(())
}

/// Installs the mod in `zip_path` into `mods_root/<id>/`.
///
/// Returns an [`AlreadyInstalled`] error (downcast it) when a mod with the
/// same id is installed and `opts.replace` is false.
pub fn install_zip(zip_path: &Path, mods_root: &Path, opts: &InstallOptions) -> Result<Installed> {
    let plan = inspect_zip(zip_path)?;
    let info = &plan.manifest.info;
    if !valid_id(&info.id) {
        bail!("the mod's id '{}' is not valid (letters, digits, '-' and '_' only)", info.id);
    }
    if let Some(want) = &opts.expect_id {
        if *want != info.id {
            bail!("the download contains the mod '{}', not '{want}'", info.id);
        }
    }
    std::fs::create_dir_all(mods_root)?;
    let existing = same_id_dirs(mods_root, &info.id);
    let replaced = existing.iter().find_map(|(_, m)| m.as_ref().map(|m| m.info.version.clone()));
    if !existing.is_empty() && !opts.replace {
        let name = existing.iter().find_map(|(_, m)| m.as_ref().map(|m| m.info.name.clone())).unwrap_or_else(|| info.name.clone());
        return Err(AlreadyInstalled {
            id: info.id.clone(),
            name,
            installed_version: replaced.clone().unwrap_or_else(|| "?".into()),
            new_version: info.version.clone(),
        }
        .into());
    }
    let suffix = unique_suffix();
    let staging = mods_root.join(format!(".incoming-{}-{suffix}", info.id));
    let result = (|| -> Result<Installed> {
        extract(zip_path, &plan.prefix, &staging)?;
        let manifest = ModManifest::load(&staging.join("mod.toml"))?;
        let problems = manifest.validate(&staging);
        if !problems.is_empty() {
            bail!("the mod has problems and was not installed:\n• {}", problems.join("\n• "));
        }
        // Move every older copy aside, then swap the new one in.
        let mut aside: Vec<(PathBuf, PathBuf)> = Vec::new();
        for (k, (dir, _)) in existing.iter().enumerate() {
            let to = mods_root.join(format!(".replaced-{}-{suffix}-{k}", info.id));
            if let Err(e) = std::fs::rename(dir, &to) {
                for (from, to) in aside.iter().rev() {
                    let _ = std::fs::rename(to, from);
                }
                return Err(anyhow!("could not replace {} ({e}); is a file in it open?", dir.display()));
            }
            aside.push((dir.clone(), to));
        }
        let target = mods_root.join(&manifest.info.id);
        if let Err(e) = std::fs::rename(&staging, &target) {
            for (from, to) in aside.iter().rev() {
                let _ = std::fs::rename(to, from);
            }
            return Err(anyhow!("could not install into {} ({e})", target.display()));
        }
        for (_, to) in aside {
            if let Err(e) = std::fs::remove_dir_all(&to) {
                log::warn!("could not remove the old copy at {}: {e}", to.display());
            }
        }
        Ok(Installed {
            id: manifest.info.id.clone(),
            name: manifest.info.name.clone(),
            version: manifest.info.version.clone(),
            dir: target,
            replaced: replaced.clone(),
            manifest,
        })
    })();
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

/// Removes an installed mod folder. Refuses anything outside `mods_root`.
pub fn uninstall(mods_root: &Path, mod_dir: &Path) -> Result<()> {
    let root = mods_root.canonicalize().context("mods folder")?;
    let dir = mod_dir.canonicalize().with_context(|| format!("{}", mod_dir.display()))?;
    if !dir.starts_with(&root) || dir == root {
        bail!("{} is not inside the mods folder", mod_dir.display());
    }
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))
}

/// Removes leftovers of interrupted installs (`.incoming-*`, `.replaced-*`).
pub fn clean_leftovers(mods_root: &Path) {
    let Ok(rd) = std::fs::read_dir(mods_root) else { return };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if (n.starts_with(".incoming-") || n.starts_with(".replaced-")) && e.path().is_dir() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

fn walk(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let mut v: Vec<_> = std::fs::read_dir(dir)?.flatten().collect();
    v.sort_by_key(|e| e.file_name());
    for e in v {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "target" || name.ends_with(".zip") {
            continue;
        }
        let p = e.path();
        let ft = e.file_type()?;
        if ft.is_symlink() {
            continue;
        } else if ft.is_dir() {
            walk(&p, base, out)?;
        } else {
            out.push(p.strip_prefix(base)?.to_path_buf());
        }
    }
    Ok(())
}

/// Packs a mod folder into `out_dir/<id>-<version>.zip` with the files under
/// a `<id>/` folder — the asset name the launcher looks for in a GitHub
/// release. Hidden files, `target/` and other `.zip` files are left out.
pub fn pack(dir: &Path, out_dir: &Path) -> Result<PathBuf> {
    use zip::write::SimpleFileOptions;
    let manifest = ModManifest::load(&dir.join("mod.toml"))?;
    let problems = manifest.validate(dir);
    if !problems.is_empty() {
        bail!("fix these first:\n• {}", problems.join("\n• "));
    }
    let id = &manifest.info.id;
    let mut files = Vec::new();
    walk(dir, dir, &mut files)?;
    std::fs::create_dir_all(out_dir)?;
    let out = out_dir.join(format!("{id}-{}.zip", manifest.info.version));
    let mut w = zip::ZipWriter::new(File::create(&out)?);
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    for rel in files {
        let name = format!("{id}/{}", rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"));
        w.start_file(name, opts)?;
        let mut f = File::open(dir.join(&rel))?;
        std::io::copy(&mut f, &mut w)?;
    }
    w.finish()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dwrc-pkg-{name}-{}", unique_suffix()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn manifest(id: &str, version: &str, script: bool) -> String {
        format!(
            "[mod]\nid = \"{id}\"\nname = \"Mod {id}\"\nversion = \"{version}\"\ngame = \"any\"\n{}",
            if script { "script = \"main.lua\"\n" } else { "" }
        )
    }

    fn make_zip(path: &Path, files: &[(&str, &str)]) {
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        for (n, c) in files {
            w.start_file(*n, SimpleFileOptions::default()).unwrap();
            w.write_all(c.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn finds_the_mod_folder() {
        let d = tmp("find");
        let z = d.join("a.zip");
        make_zip(&z, &[("mod.toml", &manifest("rootmod", "1.0.0", false))]);
        assert_eq!(inspect_zip(&z).unwrap().prefix, "");
        make_zip(&z, &[("my-mod/mod.toml", &manifest("m", "1.0.0", true)), ("my-mod/main.lua", "-- hi"), ("README.txt", "x")]);
        let p = inspect_zip(&z).unwrap();
        assert_eq!((p.prefix.as_str(), p.files), ("my-mod", 2));
        // GitHub "Download ZIP" of a repository: repo-main/<mod files>.
        make_zip(&z, &[("repo-main/mod.toml", &manifest("gh", "1.0.0", false)), ("__MACOSX/._mod.toml", "junk")]);
        assert_eq!(inspect_zip(&z).unwrap().prefix, "repo-main");
        make_zip(&z, &[("a/mod.toml", &manifest("a", "1.0.0", false)), ("b/mod.toml", &manifest("b", "1.0.0", false))]);
        assert!(inspect_zip(&z).unwrap_err().to_string().contains("more than one mod"));
        make_zip(&z, &[("readme.md", "no mod here")]);
        assert!(inspect_zip(&z).unwrap_err().to_string().contains("no mod.toml"));
        make_zip(&z, &[("m/mod.toml", &manifest("m", "1.0.0", false)), ("../../evil.txt", "x")]);
        assert!(inspect_zip(&z).unwrap_err().to_string().contains("unsafe path"));
        std::fs::write(&z, b"not a zip").unwrap();
        assert!(inspect_zip(&z).unwrap_err().to_string().contains("not a valid .zip"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn install_replace_and_rollback() {
        let d = tmp("install");
        let mods = d.join("mods");
        let z = d.join("m.zip");
        make_zip(&z, &[("cool/mod.toml", &manifest("cool", "1.0.0", true)), ("cool/main.lua", "dwrc.log('v1')"), ("cool/files/A.BIN", "data")]);
        let r = install_zip(&z, &mods, &InstallOptions::default()).unwrap();
        assert_eq!((r.id.as_str(), r.version.as_str(), r.replaced.as_deref()), ("cool", "1.0.0", None));
        assert!(mods.join("cool/files/A.BIN").is_file());
        // Same id again: refused unless replacing.
        make_zip(&z, &[("cool/mod.toml", &manifest("cool", "1.1.0", true)), ("cool/main.lua", "dwrc.log('v2')")]);
        let e = install_zip(&z, &mods, &InstallOptions::default()).unwrap_err();
        let a = e.downcast_ref::<AlreadyInstalled>().unwrap();
        assert_eq!((a.installed_version.as_str(), a.new_version.as_str()), ("1.0.0", "1.1.0"));
        let r = install_zip(&z, &mods, &InstallOptions { replace: true, ..Default::default() }).unwrap();
        assert_eq!((r.version.as_str(), r.replaced.as_deref()), ("1.1.0", Some("1.0.0")));
        assert!(!mods.join("cool/files").exists(), "the old copy's files are gone");
        assert_eq!(std::fs::read_to_string(mods.join("cool/main.lua")).unwrap(), "dwrc.log('v2')");
        // A broken update (script missing) leaves the installed copy alone.
        make_zip(&z, &[("cool/mod.toml", &manifest("cool", "2.0.0", true))]);
        let e = install_zip(&z, &mods, &InstallOptions { replace: true, ..Default::default() }).unwrap_err();
        assert!(e.to_string().contains("not installed"), "{e}");
        assert_eq!(ModManifest::load(&mods.join("cool/mod.toml")).unwrap().info.version, "1.1.0");
        // Wrong mod from a listing.
        let e = install_zip(&z, &mods, &InstallOptions { expect_id: Some("other".into()), replace: true }).unwrap_err();
        assert!(e.to_string().contains("not 'other'"));
        // No staging leftovers.
        let names: Vec<String> = std::fs::read_dir(&mods).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert_eq!(names, vec!["cool".to_string()]);
        // An older per-game copy of the same id is replaced too.
        std::fs::create_dir_all(mods.join("any/old-folder")).unwrap();
        std::fs::write(mods.join("any/old-folder/mod.toml"), manifest("legacy", "0.1.0", false)).unwrap();
        make_zip(&z, &[("legacy/mod.toml", &manifest("legacy", "0.2.0", false))]);
        let r = install_zip(&z, &mods, &InstallOptions { replace: true, ..Default::default() }).unwrap();
        assert_eq!(r.replaced.as_deref(), Some("0.1.0"));
        assert!(!mods.join("any/old-folder").exists() && mods.join("legacy/mod.toml").is_file());
        // Uninstall stays inside the mods folder.
        uninstall(&mods, &mods.join("cool")).unwrap();
        assert!(!mods.join("cool").exists());
        assert!(uninstall(&mods, &d).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn pack_round_trip() {
        let d = tmp("pack");
        let src = d.join("src");
        std::fs::create_dir_all(src.join("files/DIR")).unwrap();
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::write(src.join("mod.toml"), manifest("packed", "1.2.3", true)).unwrap();
        std::fs::write(src.join("main.lua"), "-- x").unwrap();
        std::fs::write(src.join("files/DIR/F.BIN"), "abc").unwrap();
        std::fs::write(src.join(".git/HEAD"), "ref").unwrap();
        let z = pack(&src, &d.join("out")).unwrap();
        assert_eq!(z.file_name().unwrap(), "packed-1.2.3.zip");
        let p = inspect_zip(&z).unwrap();
        assert_eq!((p.prefix.as_str(), p.files), ("packed", 3));
        let r = install_zip(&z, &d.join("mods"), &InstallOptions::default()).unwrap();
        assert_eq!(std::fs::read_to_string(r.dir.join("files/DIR/F.BIN")).unwrap(), "abc");
        let _ = std::fs::remove_dir_all(&d);
    }
}
