//! Version numbers and ranges (semver, read leniently: `v1.2`, `1.2` and
//! `1.2.0` are the same version).

use semver::{Version, VersionReq};

/// The running Re:Compiled version, which mod `engine` ranges are checked
/// against.
pub const RECOMPILED_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Parses `1`, `1.2`, `v1.2.3`, `1.2.3-beta.1` and the like.
pub fn parse(s: &str) -> Option<Version> {
    let s = s.trim();
    let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
    if let Ok(v) = Version::parse(s) {
        return Some(v);
    }
    // Pad a short core version ("1" / "1.2"), keeping any pre-release part.
    let (core, rest) = match s.find(['-', '+']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.is_empty() || parts.len() > 3 || parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let mut padded = parts.join(".");
    for _ in parts.len()..3 {
        padded.push_str(".0");
    }
    Version::parse(&format!("{padded}{rest}")).ok()
}

/// `a` compared with `b`; unparsable versions sort first.
pub fn compare(a: &str, b: &str) -> std::cmp::Ordering {
    match (parse(a), parse(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => a.cmp(b),
    }
}

/// Whether `candidate` is strictly newer than `installed`.
pub fn is_newer(candidate: &str, installed: &str) -> bool {
    matches!((parse(candidate), parse(installed)), (Some(c), Some(i)) if c > i)
}

fn req(range: &str) -> Option<VersionReq> {
    let r = range.trim();
    if r.is_empty() || r == "*" {
        return Some(VersionReq::STAR);
    }
    VersionReq::parse(r).ok().or_else(|| {
        // Accept space-separated comparators (">=0.2 <1.0") as well as commas.
        let joined = r.split_whitespace().collect::<Vec<_>>().join(", ");
        VersionReq::parse(&joined).ok()
    })
}

pub fn valid_range(range: &str) -> bool {
    req(range).is_some()
}

/// Whether `version` falls in `range`. An unreadable version or range is
/// treated as satisfied: the check is advisory, never a reason to hide a mod.
pub fn satisfies(version: &str, range: &str) -> bool {
    match (parse(version), req(range)) {
        (Some(mut v), Some(r)) => {
            // Ranges are about releases; let a pre-release of the running
            // build count as that release.
            v.pre = semver::Prerelease::EMPTY;
            r.matches(&v)
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lenient_versions() {
        assert_eq!(parse("v1.2").unwrap(), Version::new(1, 2, 0));
        assert_eq!(parse("3").unwrap(), Version::new(3, 0, 0));
        assert!(parse("1.2.3-beta.1").unwrap().pre.as_str() == "beta.1");
        assert!(parse("1.x").is_none());
        assert!(is_newer("1.10.0", "1.9.3"));
        assert!(!is_newer("1.0", "1.0.0"));
        assert!(satisfies("0.2.0", ">=0.2"));
        assert!(satisfies("0.2.5", ">=0.2 <0.3"));
        // 1.0 keeps mods written for 0.2 working, and is offered as an update.
        assert!(satisfies("1.0.0", ">=0.2") && satisfies("1.0.0", ">=0.2.6") && !satisfies("1.0.0", ">=0.2 <0.3"));
        assert!(is_newer("1.0.0", "0.2.10") && is_newer("0.2.10", "0.2.9"));
        assert!(!satisfies("0.1.9", ">=0.2"));
        assert!(satisfies("0.3.0-dev", ">=0.3"));
        assert!(valid_range("^1.2") && !valid_range(">=banana"));
    }
}
