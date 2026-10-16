//! Mod indexes (the launcher's **Find mods** tab).
//!
//! An index is a GitHub repository that publishes one JSON feed listing mods
//! that live in their authors' own repositories. The index only says where
//! each mod's `.zip` is; installing goes through exactly the same path as
//! importing a `.zip` by hand, so a listing grants a mod no extra trust.
//!
//! Players add an index by pasting `owner/repo`, the repository's GitHub
//! URL, its Pages URL, or the feed URL itself; all resolve to the same
//! source. The feed is read from GitHub Pages
//! (`https://<owner>.github.io/<repo>/data/index.json`), falling back to the
//! `gh-pages` branch on raw.githubusercontent.com when Pages is slow to
//! deploy. The built-in index is always present and cannot be removed.
//!
//! `schema_version` is a hard gate: a feed with a different version is
//! refused rather than half-read. New fields are only ever added.

use dwrc_disc::GameId;
use dwrc_mods::version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// The Re:Compiled community index, listed for every player.
pub const BUILTIN_INDEX: &str = "Jack-The-Gamer/DigimonWorldsReCompiled-mod-index";

/// The feed version this build reads.
pub const SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexSource {
    /// What the player typed (shown back to them).
    pub input: String,
    pub feed: String,
    /// What relative thumbnail/description paths resolve against.
    pub base: String,
    /// Mirror of the same files, tried when the feed cannot be fetched.
    pub fallback: Option<(String, String)>,
    /// Short name: `owner/repo` or the host.
    pub label: String,
    pub builtin: bool,
}

fn github_slug(url: &str) -> Option<(String, String)> {
    let u = url.trim().trim_end_matches('/');
    let rest = u
        .strip_prefix("https://github.com/")
        .or_else(|| u.strip_prefix("http://github.com/"))
        .or_else(|| u.strip_prefix("https://www.github.com/"))
        .or_else(|| u.strip_prefix("github.com/"));
    let rest = match rest {
        Some(r) => r,
        None if !u.contains("://") && !u.contains(' ') => u,
        None => return None,
    };
    let repo = dwrc_mods::parse_github_repo(rest.split("/tree/").next().unwrap_or(rest))?;
    let (o, r) = repo.split_once('/')?;
    Some((o.to_string(), r.to_string()))
}

/// Resolves what a player pasted into an index source.
pub fn resolve_source(input: &str) -> Result<IndexSource, String> {
    let url = input.trim();
    if url.is_empty() {
        return Err("enter an index: owner/repo, a GitHub link or a feed URL".into());
    }
    if let Some((owner, repo)) = github_slug(url) {
        let host = format!("{}.github.io", owner.to_ascii_lowercase());
        let base = if repo.eq_ignore_ascii_case(&host) { format!("https://{host}/") } else { format!("https://{host}/{repo}/") };
        let raw = format!("https://raw.githubusercontent.com/{owner}/{repo}/gh-pages/");
        return Ok(IndexSource {
            input: url.to_string(),
            feed: format!("{base}data/index.json"),
            fallback: Some((format!("{raw}data/index.json"), raw)),
            base,
            label: format!("{owner}/{repo}"),
            builtin: false,
        });
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("an index must be owner/repo, a GitHub link, or an http(s) URL".into());
    }
    let (feed, base) = if url.ends_with(".json") {
        let base = url.strip_suffix("data/index.json").map(str::to_string).unwrap_or_else(|| url[..url.rfind('/').unwrap() + 1].to_string());
        (url.to_string(), base)
    } else {
        let base = if url.ends_with('/') { url.to_string() } else { format!("{url}/") };
        (format!("{base}data/index.json"), base)
    };
    Ok(IndexSource { input: url.to_string(), label: label_for(&base), feed, base, fallback: None, builtin: false })
}

/// `owner/repo` for a Pages host, else host plus the first path segment.
pub fn label_for(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let host = parts.next().unwrap_or("");
    let first = parts.next();
    if let (Some(owner), Some(repo)) = (host.strip_suffix(".github.io"), first) {
        return format!("{owner}/{repo}");
    }
    match first {
        Some(f) => format!("{host}/{f}"),
        None => host.to_string(),
    }
}

/// The built-in index followed by the player's, without duplicates.
pub fn sources(user: &[String]) -> Vec<IndexSource> {
    let mut b = resolve_source(BUILTIN_INDEX).expect("built-in index resolves");
    b.builtin = true;
    let mut out = vec![b];
    for u in user {
        if let Ok(s) = resolve_source(u) {
            if !out.iter().any(|o| o.feed == s.feed) {
                out.push(s);
            }
        }
    }
    out
}

/// Joins a feed-relative path onto a base URL (absolute URLs pass through).
pub fn join_url(base: &str, rel: &str) -> Option<String> {
    let rel = rel.trim();
    if rel.is_empty() {
        return None;
    }
    if rel.starts_with("https://") || rel.starts_with("http://") {
        return Some(rel.to_string());
    }
    let base = if base.ends_with('/') { base.to_string() } else { format!("{base}/") };
    Some(format!("{base}{}", rel.trim_start_matches('/')))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Latest {
    pub version: Option<String>,
    pub tag: Option<String>,
    pub prerelease: bool,
    pub published_at: Option<String>,
    pub zip: Option<crate::github::Asset>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Downloads {
    pub total: Option<u64>,
    pub recent: Option<u64>,
    pub window_days: Option<u64>,
    pub as_of: Option<String>,
}

/// One mod listed in an index.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexEntry {
    pub folder: Option<String>,
    pub id: String,
    pub title: String,
    pub author: Option<String>,
    pub summary: String,
    pub version: Option<String>,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    /// `dw1`, `dw2`, `dw3` or `any`.
    pub games: Vec<String>,
    pub repo: Option<String>,
    pub github: Option<String>,
    pub download_url: Option<String>,
    pub license: Option<String>,
    pub engine: Option<String>,
    pub experimental: bool,
    pub dependencies: Vec<String>,
    pub conflicts: Vec<String>,
    pub thumbnail: Option<String>,
    pub description_url: Option<String>,
    pub latest: Option<Latest>,
    /// `ok`, `off`, `no installable release`, `pending` or `error: …`.
    pub update_check: String,
    pub downloads: Option<Downloads>,
    pub first_release: Option<String>,
    pub last_release: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Index {
    pub generated_at: Option<String>,
    pub categories: Vec<String>,
    pub mods: Vec<IndexEntry>,
}

fn st(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string).filter(|s| !s.is_empty())
}

fn arr(v: &Value, k: &str) -> Vec<String> {
    v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn num(v: &Value, k: &str) -> Option<u64> {
    v.get(k).and_then(|x| x.as_u64().or_else(|| x.as_f64().map(|f| f as u64)).or_else(|| x.as_str().and_then(|s| s.parse().ok())))
}

fn parse_entry(v: &Value) -> Option<IndexEntry> {
    let id = st(v, "id")?;
    if !dwrc_mods::valid_id(&id) {
        return None;
    }
    let latest = v.get("latest").filter(|l| l.is_object()).map(|l| Latest {
        version: st(l, "version"),
        tag: st(l, "tag"),
        prerelease: l.get("prerelease").and_then(Value::as_bool).unwrap_or(false),
        published_at: st(l, "published_at"),
        zip: l.get("zip").and_then(|z| Some(crate::github::Asset { name: st(z, "name").unwrap_or_default(), url: st(z, "url")?, size: num(z, "size") })),
    });
    let downloads = match v.get("downloads") {
        Some(Value::Number(n)) => n.as_u64().map(|t| Downloads { total: Some(t), ..Default::default() }),
        Some(d @ Value::Object(_)) => Some(Downloads { total: num(d, "total"), recent: num(d, "recent"), window_days: num(d, "window_days"), as_of: st(d, "as_of") })
            .filter(|d| d.total.is_some() || d.recent.is_some()),
        _ => None,
    };
    let deps = |k: &str| match v.get(k) {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        Some(Value::Object(o)) => o.iter().map(|(id, r)| match r.as_str() {
            Some(r) if !r.is_empty() && r != "*" => format!("{id}@{r}"),
            _ => id.clone(),
        }).collect(),
        _ => Vec::new(),
    };
    Some(IndexEntry {
        folder: st(v, "folder"),
        title: st(v, "title").unwrap_or_else(|| id.clone()),
        author: st(v, "author"),
        summary: st(v, "summary").unwrap_or_default(),
        version: st(v, "version"),
        categories: arr(v, "categories"),
        tags: arr(v, "tags"),
        games: arr(v, "games"),
        repo: st(v, "repo"),
        github: st(v, "github"),
        download_url: st(v, "downloadURL").or_else(|| st(v, "download_url")),
        license: st(v, "license"),
        engine: st(v, "engine"),
        experimental: v.get("experimental").and_then(Value::as_bool).unwrap_or(false),
        dependencies: deps("dependencies"),
        conflicts: deps("conflicts"),
        thumbnail: st(v, "thumbnail"),
        description_url: st(v, "description_url"),
        latest,
        update_check: st(v, "update_check").unwrap_or_else(|| "pending".into()),
        downloads,
        first_release: st(v, "first_release"),
        last_release: st(v, "last_release"),
        id,
    })
}

/// Parses a feed. Never panics: a truncated download, an HTML error page
/// or a feed from a newer schema all come back as a readable message.
pub fn parse(json: &str) -> Result<Index, String> {
    let t = json.trim_start();
    if t.starts_with('<') {
        return Err("the index URL returned a web page, not index.json".into());
    }
    let doc: Value = serde_json::from_str(t).map_err(|e| format!("index.json could not be read ({e})"))?;
    let Some(schema) = doc.get("schema_version").and_then(Value::as_u64) else {
        return Err("index.json has no schema_version".into());
    };
    if schema != SCHEMA_VERSION {
        return Err(format!("this index uses feed version {schema}; this build of Re:Compiled reads version {SCHEMA_VERSION}"));
    }
    let Some(mods) = doc.get("mods").and_then(Value::as_array) else {
        return Err("index.json has no mods list".into());
    };
    let mut seen = std::collections::HashSet::new();
    let mods = mods.iter().filter_map(parse_entry).filter(|e| seen.insert(e.id.clone())).collect();
    Ok(Index { generated_at: st(&doc, "generated_at"), categories: arr(&doc, "categories"), mods })
}

impl IndexEntry {
    /// The `.zip` to install, and whether it is a tracked release.
    pub fn install_url(&self) -> Result<String, String> {
        if self.update_check == "ok" {
            if let Some(z) = self.latest.as_ref().and_then(|l| l.zip.as_ref()) {
                return Ok(z.url.clone());
            }
        }
        if let Some(u) = &self.download_url {
            return Ok(u.clone());
        }
        Err(match self.update_check.as_str() {
            "off" => "the author does not publish installable releases".into(),
            "no installable release" => "no release with a .zip yet".into(),
            e if e.starts_with("error") => e.to_string(),
            _ => "nothing installable is listed yet".into(),
        })
    }

    /// The version a card shows: the release the index resolved, else the
    /// listed one.
    pub fn display_version(&self) -> String {
        if self.update_check == "ok" {
            if let Some(v) = self.latest.as_ref().and_then(|l| l.version.clone()) {
                return v;
            }
        }
        self.version.clone().unwrap_or_else(|| "?".into())
    }

    pub fn game_ids(&self) -> Vec<GameId> {
        let sels: Vec<dwrc_mods::GameSel> = self.games.iter().filter_map(|g| dwrc_mods::GameSel::parse(g)).collect();
        if sels.is_empty() {
            return GameId::ALL.to_vec();
        }
        GameId::ALL.into_iter().filter(|g| sels.iter().any(|s| s.matches(*g))).collect()
    }

    pub fn total_downloads(&self) -> Option<u64> {
        self.downloads.as_ref().and_then(|d| d.total)
    }

    pub fn recent_downloads(&self) -> Option<u64> {
        self.downloads.as_ref().and_then(|d| d.recent)
    }

    /// `YYYY-MM-DD` of the newest release, if known.
    pub fn updated(&self) -> Option<String> {
        self.last_release
            .clone()
            .or_else(|| self.latest.as_ref().and_then(|l| l.published_at.clone()))
            .map(|d| d.chars().take(10).collect())
    }

    /// Every search term must appear in the title, author, summary or id.
    pub fn matches(&self, query: &str) -> bool {
        let hay = format!("{} {} {} {} {}", self.title, self.author.as_deref().unwrap_or(""), self.summary, self.id, self.tags.join(" ")).to_lowercase();
        query.split_whitespace().all(|t| hay.contains(&t.to_lowercase()))
    }
}

/// What the player has, for compatibility warnings.
#[derive(Debug, Clone, Default)]
pub struct CompatContext {
    /// Installed mod id -> version.
    pub installed: HashMap<String, String>,
    /// Games whose discs are imported.
    pub imported: Vec<GameId>,
}

/// Soft warnings shown before an install. Nothing here hides a mod: an
/// index entry is metadata an author wrote, possibly long ago.
pub fn compat_issues(e: &IndexEntry, ctx: &CompatContext) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(r) = &e.engine {
        if !version::satisfies(version::RECOMPILED_VERSION, r) {
            out.push(format!("Needs Re:Compiled {r} (you have {})", version::RECOMPILED_VERSION));
        }
    }
    if e.experimental {
        out.push("Marked experimental by its author".into());
    }
    for d in e.dependencies.iter().filter_map(|d| dwrc_mods::ModRef::parse(d)) {
        match ctx.installed.get(&d.id) {
            None => out.push(format!("Needs the mod {d} (not installed)")),
            Some(v) if !d.accepts(v) => out.push(format!("Needs the mod {d} (you have v{v})")),
            _ => {}
        }
    }
    for c in e.conflicts.iter().filter_map(|c| dwrc_mods::ModRef::parse(c)) {
        if ctx.installed.get(&c.id).is_some_and(|v| c.accepts(v)) {
            out.push(format!("Cannot run alongside {}, which is installed", c.id));
        }
    }
    let games = e.game_ids();
    if !ctx.imported.is_empty() && !games.iter().any(|g| ctx.imported.contains(g)) {
        let names: Vec<&str> = games.iter().map(|g| g.title()).collect();
        out.push(format!("Made for {}, which you have not imported yet", names.join(" / ")));
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Name,
    MostDownloaded,
    Trending,
    RecentlyUpdated,
}

impl SortKey {
    pub const ALL: [SortKey; 4] = [SortKey::Name, SortKey::MostDownloaded, SortKey::Trending, SortKey::RecentlyUpdated];
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Name => "Name",
            SortKey::MostDownloaded => "Most downloaded",
            SortKey::Trending => "Trending",
            SortKey::RecentlyUpdated => "Recently updated",
        }
    }
}

/// Sorts entries; entries without the sorted-on number go last.
pub fn sort(entries: &mut [&IndexEntry], key: SortKey) {
    let name = |e: &IndexEntry| e.title.to_lowercase();
    match key {
        SortKey::Name => entries.sort_by_key(|e| name(e)),
        SortKey::MostDownloaded => entries.sort_by(|a, b| b.total_downloads().cmp(&a.total_downloads()).then_with(|| name(a).cmp(&name(b)))),
        SortKey::Trending => entries.sort_by(|a, b| b.recent_downloads().cmp(&a.recent_downloads()).then_with(|| b.total_downloads().cmp(&a.total_downloads()))),
        SortKey::RecentlyUpdated => entries.sort_by(|a, b| b.updated().cmp(&a.updated()).then_with(|| name(a).cmp(&name(b)))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_resolve() {
        for input in ["Some-Org/mod-index", "https://github.com/Some-Org/mod-index", "https://github.com/Some-Org/mod-index.git", "github.com/Some-Org/mod-index/"] {
            let s = resolve_source(input).unwrap();
            assert_eq!(s.feed, "https://some-org.github.io/mod-index/data/index.json", "{input}");
            assert_eq!(s.label, "Some-Org/mod-index");
            assert_eq!(s.fallback.unwrap().0, "https://raw.githubusercontent.com/Some-Org/mod-index/gh-pages/data/index.json");
        }
        let s = resolve_source("https://some-org.github.io/mod-index/").unwrap();
        assert_eq!((s.feed.as_str(), s.label.as_str()), ("https://some-org.github.io/mod-index/data/index.json", "some-org/mod-index"));
        let s = resolve_source("https://example.com/feeds/dw.json").unwrap();
        assert_eq!((s.feed.as_str(), s.base.as_str()), ("https://example.com/feeds/dw.json", "https://example.com/feeds/"));
        let s = resolve_source("http://127.0.0.1:8080").unwrap();
        assert_eq!(s.feed, "http://127.0.0.1:8080/data/index.json");
        assert!(resolve_source("not a url at all").is_err());
        let all = sources(&[BUILTIN_INDEX.into(), "a/b".into(), "https://github.com/a/b".into()]);
        assert_eq!(all.len(), 2);
        assert!(all[0].builtin && !all[1].builtin);
        assert_eq!(join_url("https://h/x/", "data/mods/a/t.png").unwrap(), "https://h/x/data/mods/a/t.png");
        assert_eq!(join_url("https://h/x/", "https://cdn/t.png").unwrap(), "https://cdn/t.png");
    }

    #[test]
    fn feed_parsing() {
        let feed = r#"{"schema_version":1,"generated_at":"2026-10-01T00:00:00Z","categories":["QOL"],"mods":[
          {"id":"turbo","title":"Turbo","author":"A","summary":"fast text","version":"1.0.0","games":["any"],
           "latest":{"version":"1.2.0","zip":{"name":"turbo-1.2.0.zip","url":"https://x/t.zip"}},"update_check":"ok",
           "downloads":{"total":50,"recent":9},"dependencies":{"core":">=1"}},
          {"id":"fixed","title":"Fixed","downloadURL":"https://x/f.zip","version":"0.3","games":["dw2"],"downloads":7},
          {"id":"gone","title":"Gone","update_check":"error: repository not found"},
          {"id":"bad id!"}, {"title":"no id"}, {"id":"turbo","title":"dup"}
        ]}"#;
        let ix = parse(feed).unwrap();
        assert_eq!(ix.mods.len(), 3);
        let t = &ix.mods[0];
        assert_eq!((t.install_url().unwrap().as_str(), t.display_version().as_str()), ("https://x/t.zip", "1.2.0"));
        assert_eq!(t.dependencies, vec!["core@>=1"]);
        assert_eq!(t.game_ids().len(), 3);
        let f = &ix.mods[1];
        assert_eq!((f.install_url().unwrap().as_str(), f.display_version().as_str(), f.total_downloads()), ("https://x/f.zip", "0.3", Some(7)));
        assert_eq!(f.game_ids(), vec![GameId::Dw2]);
        assert!(ix.mods[2].install_url().unwrap_err().contains("not found"));
        assert!(parse(r#"{"schema_version":2,"mods":[]}"#).unwrap_err().contains("version 2"));
        assert!(parse("<!doctype html><html>").unwrap_err().contains("web page"));
        assert!(parse("{\"mods\":[]}").is_err());
        // Search, sort, compatibility.
        assert!(t.matches("TURBO a") && !t.matches("turbo zzz"));
        let mut v: Vec<&IndexEntry> = ix.mods.iter().collect();
        sort(&mut v, SortKey::MostDownloaded);
        assert_eq!(v.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["turbo", "fixed", "gone"]);
        let ctx = CompatContext { installed: [("core".to_string(), "0.9.0".to_string())].into(), imported: vec![GameId::Dw1] };
        let w = compat_issues(t, &ctx);
        assert!(w.iter().any(|s| s.contains("core >=1") && s.contains("0.9.0")), "{w:?}");
        let w = compat_issues(f, &ctx);
        assert!(w.iter().any(|s| s.contains("not imported")), "{w:?}");
    }
}
