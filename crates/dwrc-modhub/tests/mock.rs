//! End-to-end checks against a local HTTP server standing in for GitHub
//! Pages, the GitHub API and release downloads.

use dwrc_modhub::{index, Hub, Progress};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

type Routes = Arc<Mutex<HashMap<String, (u16, Vec<(String, String)>, Vec<u8>)>>>;

fn serve() -> (String, Routes, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", l.local_addr().unwrap());
    let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let (r2, log2) = (routes.clone(), log.clone());
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut rd = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            if rd.read_line(&mut line).is_err() {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            let mut ua = String::new();
            loop {
                let mut h = String::new();
                if rd.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
                if h.to_ascii_lowercase().starts_with("user-agent:") {
                    ua = h[11..].trim().to_string();
                }
            }
            log2.lock().unwrap().push(format!("{path} {ua}"));
            let (code, headers, body) = r2.lock().unwrap().get(&path).cloned().unwrap_or((404, vec![], b"{\"message\":\"Not Found\"}".to_vec()));
            let mut s = s;
            let mut head = format!("HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
            for (k, v) in headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(&body);
        }
    });
    (addr, routes, log)
}

fn set(routes: &Routes, path: &str, code: u16, body: &[u8]) {
    routes.lock().unwrap().insert(path.to_string(), (code, vec![], body.to_vec()));
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("dwrc-modhub-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn mod_zip(id: &str, version: &str) -> Vec<u8> {
    use std::io::Write;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        w.start_file(format!("{id}/mod.toml"), o).unwrap();
        write!(w, "[mod]\nid = \"{id}\"\nname = \"{id}\"\nversion = \"{version}\"\ngame = \"any\"\n").unwrap();
        w.finish().unwrap();
    }
    buf.into_inner()
}

#[test]
fn index_fetch_cache_and_fallback() {
    let (base, routes, log) = serve();
    let dir = tmp("index");
    let hub = Hub::new(dir.join("cache"));
    let feed = br#"{"schema_version":1,"mods":[{"id":"a","title":"A","downloadURL":"http://x/a.zip","version":"1.0.0"}]}"#;
    set(&routes, "/data/index.json", 200, feed);
    let src = index::resolve_source(&base).unwrap();
    let f = hub.fetch_index(&src, false);
    assert!(f.error.is_none() && !f.from_cache, "{:?}", f.error);
    assert_eq!(f.index.unwrap().mods[0].id, "a");
    assert!(log.lock().unwrap()[0].contains("DigimonWorldsReCompiled/"), "user agent sent");
    // Fresh cache: no request.
    let n = log.lock().unwrap().len();
    assert!(hub.fetch_index(&src, false).from_cache);
    assert_eq!(log.lock().unwrap().len(), n);
    // Forced refresh while the server is down: stale copy plus the error.
    set(&routes, "/data/index.json", 500, b"oops");
    let f = hub.fetch_index(&src, true);
    assert!(f.from_cache && f.error.as_deref().unwrap().contains("HTTP 500"));
    assert_eq!(f.index.unwrap().mods.len(), 1);
    // A mirror answers when the feed does not.
    let mut src2 = index::resolve_source(&format!("{base}/primary/")).unwrap();
    src2.fallback = Some((format!("{base}/mirror/data/index.json"), format!("{base}/mirror/")));
    set(&routes, "/mirror/data/index.json", 200, feed);
    let f = hub.fetch_index(&src2, false);
    assert!(f.error.is_none(), "{:?}", f.error);
    assert_eq!(f.base, format!("{base}/mirror/"));
    // An HTML page is reported as such.
    set(&routes, "/html/data/index.json", 200, b"<html>hi</html>");
    let f = hub.fetch_index(&index::resolve_source(&format!("{base}/html")).unwrap(), false);
    assert!(f.index.is_none() && f.error.unwrap().contains("web page"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn releases_update_and_install() {
    let (base, routes, _log) = serve();
    let dir = tmp("releases");
    let mut hub = Hub::new(dir.join("cache"));
    hub.http.github_api = base.clone();
    let releases = format!(
        r#"[{{"tag_name":"v1.1.0","name":"Better","body":"* faster","published_at":"2026-10-01T00:00:00Z",
             "assets":[{{"name":"cool-1.1.0.zip","browser_download_url":"{base}/dl/cool-1.1.0.zip","size":1}}]}},
            {{"tag_name":"v1.0.0","assets":[{{"name":"cool-1.0.0.zip","browser_download_url":"{base}/dl/cool-1.0.0.zip"}}]}}]"#
    );
    set(&routes, "/repos/me/cool/releases?per_page=100", 200, releases.as_bytes());
    set(&routes, "/dl/cool-1.1.0.zip", 200, &mod_zip("cool", "1.1.0"));
    let r = hub.fetch_releases("me/cool", "cool", false);
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.releases.len(), 2);
    let offer = dwrc_modhub::update_offer("1.0.0", Some(&r.releases), &[]).unwrap();
    assert_eq!((offer.version.as_str(), offer.notes.as_deref()), ("1.1.0", Some("* faster")));
    assert!(dwrc_modhub::update_offer("1.1.0", Some(&r.releases), &[]).is_none());
    // Download with progress, then install through the normal zip path.
    let p = Progress::default();
    let zip = hub.download_zip(&offer.url, &dir.join("dl"), &p).unwrap();
    assert_eq!(p.fraction(), Some(1.0));
    let mods = dir.join("mods");
    let opts = dwrc_mods::package::InstallOptions { expect_id: Some("cool".into()), replace: true };
    let done = dwrc_mods::package::install_zip(&zip, &mods, &opts).unwrap();
    assert_eq!(done.version, "1.1.0");
    // Missing repository and rate limiting read well.
    let r = hub.fetch_releases("me/missing", "x", false);
    assert!(r.error.unwrap().contains("me/missing was not found"));
    routes.lock().unwrap().insert(
        "/repos/me/limited/releases?per_page=100".into(),
        (403, vec![("x-ratelimit-remaining".into(), "0".into())], b"{}".to_vec()),
    );
    let r = hub.fetch_releases("me/limited", "x", false);
    assert!(r.error.unwrap().contains("hourly limit"));
    // A failed download leaves nothing behind.
    let e = hub.download_zip(&format!("{base}/dl/nope.zip"), &dir.join("dl2"), &Progress::default()).unwrap_err();
    assert!(e.to_string().contains("not found"));
    assert_eq!(std::fs::read_dir(dir.join("dl2")).map(|d| d.count()).unwrap_or(0), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Real TLS through the environment's proxy, when the network allows it.
#[test]
#[ignore]
fn live_tls() {
    let hub = Hub::new(std::env::temp_dir().join("dwrc-live"));
    let t = hub.http.get_text("https://index.crates.io/config.json", None).unwrap();
    assert!(t.contains("dl"));
}
