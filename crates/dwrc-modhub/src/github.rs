//! Mod releases on GitHub.
//!
//! A mod that sets `github = "owner/repo"` publishes each version as a
//! GitHub Release whose tag is the version (`v1.2.0`, `1.2.0`, or
//! `<id>-v1.2.0` when one repository holds several mods) with the mod as a
//! `.zip` asset. The asset is chosen as `<id>-<version>.zip`, then any
//! `.zip` whose name starts with the id, then the first `.zip` — the same
//! order Gen1Recomp uses, and the name `dwrc-cli mod pack` produces.

use dwrc_mods::version;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    /// The version, normalised (`1.2.0`).
    pub version: String,
    pub tag: String,
    pub name: String,
    pub prerelease: bool,
    /// Release notes (markdown).
    #[serde(default)]
    pub body: String,
    /// `YYYY-MM-DD`.
    #[serde(default)]
    pub published: Option<String>,
    /// Downloads of this release's `.zip` assets.
    #[serde(default)]
    pub downloads: u64,
    pub zip: Asset,
}

/// The version a tag names, for a mod `id`: strips `<id>-`/`<id>@` and a
/// leading `v`. `None` when the rest is not a version number.
pub fn tag_version(tag: &str, id: &str) -> Option<String> {
    let t = tag.trim();
    let t = t
        .strip_prefix(&format!("{id}-"))
        .or_else(|| t.strip_prefix(&format!("{id}@")))
        .or_else(|| t.strip_prefix(&format!("{id}_")))
        .unwrap_or(t);
    let v = version::parse(t)?;
    Some(v.to_string())
}

/// Picks the mod's `.zip` among a release's assets.
pub fn pick_zip(assets: &[Asset], id: &str, version: &str) -> Option<Asset> {
    let zips: Vec<&Asset> = assets.iter().filter(|a| a.name.to_ascii_lowercase().ends_with(".zip")).collect();
    let exact = format!("{id}-{version}.zip").to_ascii_lowercase();
    let idl = id.to_ascii_lowercase();
    zips.iter()
        .find(|a| a.name.to_ascii_lowercase() == exact)
        .or_else(|| zips.iter().find(|a| a.name.to_ascii_lowercase().starts_with(&idl)))
        .or_else(|| zips.first())
        .map(|a| (*a).clone())
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

/// Decodes `GET /repos/{repo}/releases` (or a single release object) into
/// installable releases, newest version first. Drafts, tags that are not
/// version numbers and releases without a `.zip` are left out.
pub fn parse_releases(json: &str, id: &str) -> anyhow::Result<Vec<Release>> {
    let doc: Value = serde_json::from_str(json).map_err(|e| anyhow::anyhow!("GitHub sent something unexpected ({e})"))?;
    if let Some(msg) = doc.get("message").and_then(Value::as_str) {
        if doc.get("tag_name").is_none() {
            anyhow::bail!("GitHub: {msg}");
        }
    }
    let items: Vec<&Value> = match &doc {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![&doc],
        _ => anyhow::bail!("GitHub sent something unexpected"),
    };
    let mut out = Vec::new();
    for r in items {
        if r.get("draft").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(tag) = s(r, "tag_name") else { continue };
        let Some(ver) = tag_version(&tag, id) else { continue };
        let assets: Vec<(Asset, u64)> = r
            .get("assets")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| {
                        Some((
                            Asset { name: s(x, "name")?, url: s(x, "browser_download_url")?, size: x.get("size").and_then(Value::as_u64) },
                            x.get("download_count").and_then(Value::as_u64).unwrap_or(0),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let plain: Vec<Asset> = assets.iter().map(|(a, _)| a.clone()).collect();
        let Some(zip) = pick_zip(&plain, id, &ver) else { continue };
        let downloads = assets.iter().filter(|(a, _)| a.name.to_ascii_lowercase().ends_with(".zip")).map(|(_, d)| d).sum();
        out.push(Release {
            version: ver.clone(),
            name: s(r, "name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| ver.clone()),
            tag,
            prerelease: r.get("prerelease").and_then(Value::as_bool).unwrap_or(false),
            body: s(r, "body").unwrap_or_default(),
            published: s(r, "published_at").or_else(|| s(r, "created_at")).map(|d| d.chars().take(10).collect()),
            downloads,
            zip,
        });
    }
    out.sort_by(|a, b| version::compare(&b.version, &a.version));
    Ok(out)
}

/// The release to offer: the newest stable one, else the newest.
pub fn best(releases: &[Release]) -> Option<&Release> {
    releases.iter().find(|r| !r.prerelease).or_else(|| releases.first())
}

pub fn releases_path(repo: &str) -> String {
    format!("/repos/{repo}/releases?per_page=100")
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"[
      {"tag_name":"v1.10.0","name":"","prerelease":true,"draft":false,"body":"beta","published_at":"2026-09-01T10:00:00Z",
       "assets":[{"name":"cool-1.10.0.zip","browser_download_url":"https://x/c110.zip","size":10,"download_count":3}]},
      {"tag_name":"v1.9.0","name":"Nine","prerelease":false,"body":"**nine**","published_at":"2026-08-01T10:00:00Z",
       "assets":[{"name":"source.tar.gz","browser_download_url":"https://x/s","download_count":100},
                 {"name":"other.zip","browser_download_url":"https://x/o.zip","download_count":1},
                 {"name":"cool-1.9.0.zip","browser_download_url":"https://x/c19.zip","download_count":40}]},
      {"tag_name":"nightly","assets":[{"name":"cool.zip","browser_download_url":"https://x/n.zip"}]},
      {"tag_name":"v2.0.0","draft":true,"assets":[{"name":"cool-2.0.0.zip","browser_download_url":"https://x/d.zip"}]},
      {"tag_name":"v1.2.0","assets":[{"name":"notes.txt","browser_download_url":"https://x/t"}]},
      {"tag_name":"cool-v1.1.0","assets":[{"name":"anything.zip","browser_download_url":"https://x/a.zip"}]}
    ]"#;

    #[test]
    fn parses_and_picks() {
        let r = parse_releases(JSON, "cool").unwrap();
        let v: Vec<&str> = r.iter().map(|x| x.version.as_str()).collect();
        assert_eq!(v, vec!["1.10.0", "1.9.0", "1.1.0"], "drafts, non-versions and zip-less releases dropped; semver order");
        assert_eq!(r[1].zip.url, "https://x/c19.zip");
        assert_eq!(r[1].downloads, 41);
        assert_eq!(r[1].published.as_deref(), Some("2026-08-01"));
        assert_eq!(r[0].name, "1.10.0");
        assert_eq!(best(&r).unwrap().version, "1.9.0", "stable preferred over a newer pre-release");
        assert!(parse_releases(r#"{"message":"Not Found"}"#, "x").unwrap_err().to_string().contains("Not Found"));
        assert_eq!(tag_version("V2", "x").as_deref(), Some("2.0.0"));
    }
}
