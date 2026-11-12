//! tests/unit/community-themes.test.ts: the gallery, install / update / remove, and the
//! checks on everything that comes from GitHub, against a fake GitHub.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use wraithgrid_themes::{
    list_themes, newer_version, repo_from_url, CommunityThemes, CommunityThemesDeps, FetchResponse,
    ThemeName, ThemeRadius, COMMUNITY_THEMES_URL,
};

type Files = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// A fake GitHub: files by URL; every URL asked for is recorded.
struct Fake {
    files: Files,
    asked: Arc<Mutex<Vec<String>>>,
}

fn png() -> Vec<u8> {
    let mut b = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    b.extend([0u8; 32]);
    b
}

fn raw(repo: &str, f: &str) -> String {
    format!("https://raw.githubusercontent.com/{repo}/HEAD/{f}")
}

fn manifest(o: Value) -> String {
    let mut m = json!({
        "name": "Ember",
        "author": "ana",
        "version": "1.0.0",
        "description": "Warm and dark",
        "modes": ["dark"]
    });
    for (k, v) in o.as_object().unwrap() {
        m[k] = v.clone();
    }
    m.to_string()
}

fn theme(o: Value) -> String {
    let mut t = json!({
        "name": "Ember",
        "mode": "dark",
        "colors": { "bg": "#1a1210", "text": "#f5e9e1", "accent": "#ff7a45" },
        "style": { "radius": "large" }
    });
    for (k, v) in o.as_object().unwrap() {
        t[k] = v.clone();
    }
    t.to_string()
}

fn files(extra: Vec<(String, Vec<u8>)>) -> HashMap<String, Vec<u8>> {
    let mut f: HashMap<String, Vec<u8>> = HashMap::from([
        (
            COMMUNITY_THEMES_URL.to_owned(),
            json!({ "themes": [{ "repo": "ana/ember" }, { "repo": "bob/broken" }, { "repo": "bad repo!/x" }] })
                .to_string()
                .into_bytes(),
        ),
        (raw("ana/ember", "manifest.json"), manifest(json!({})).into_bytes()),
        (raw("ana/ember", "wraithgrid-theme.json"), theme(json!({})).into_bytes()),
        (raw("ana/ember", "screenshot.png"), png()),
        (raw("bob/broken", "manifest.json"), b"{ nope".to_vec()),
    ]);
    f.extend(extra);
    f
}

fn fake(files: HashMap<String, Vec<u8>>) -> Fake {
    Fake {
        files: Arc::new(Mutex::new(files)),
        asked: Arc::new(Mutex::new(Vec::new())),
    }
}

fn community(f: &Fake, dir: &Path, now: Option<Arc<AtomicI64>>) -> CommunityThemes {
    let (files, asked) = (f.files.clone(), f.asked.clone());
    CommunityThemes::new(CommunityThemesDeps {
        fetch: Box::new(move |req| {
            asked.lock().unwrap().push(req.url.to_owned());
            Ok(match files.lock().unwrap().get(req.url) {
                Some(body) => FetchResponse {
                    status: 200,
                    content_length: None,
                    body: body.clone(),
                },
                None => FetchResponse {
                    status: 404,
                    content_length: None,
                    body: b"404".to_vec(),
                },
            })
        }),
        dir: dir.to_owned(),
        app_version: "1.15.0".into(),
        now: Box::new(move || now.as_ref().map_or(0, |n| n.load(Ordering::SeqCst))),
    })
}

fn themes_dir() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new().prefix("wg-ct-").tempdir().unwrap();
    let dir = root.path().join("themes");
    fs::create_dir(&dir).unwrap();
    (root, dir)
}

#[test]
fn lists_themes_with_a_valid_manifest_bad_entries_and_repos_are_left_out() {
    let (_root, dir) = themes_dir();
    let bad = fake(files(vec![]));
    // The list itself refuses a bad repo name, so the whole list must be valid.
    let ok = fake(files(vec![(
        COMMUNITY_THEMES_URL.to_owned(),
        json!({ "themes": [{ "repo": "ana/ember" }, { "repo": "bob/broken" }] })
            .to_string()
            .into_bytes(),
    )]));
    let r = community(&ok, &dir, None).list().unwrap();
    assert_eq!(r.themes.len(), 1);
    let t = &r.themes[0];
    assert_eq!(t.repo, "ana/ember");
    assert_eq!(t.name, "Ember");
    assert_eq!(t.author, "ana");
    assert_eq!(t.modes, [ThemeName::Dark]);
    assert_eq!(t.installed_id, None);
    assert!(!t.update && !t.too_new);
    let e = community(&bad, &dir, None).list().unwrap_err();
    assert_eq!(
        e.to_string(),
        "Could not read the community theme list. Check your connection."
    );
    // Only GitHub's raw host, over https.
    for u in bad
        .asked
        .lock()
        .unwrap()
        .iter()
        .chain(ok.asked.lock().unwrap().iter())
    {
        assert!(u.starts_with("https://raw.githubusercontent.com/"), "{u}");
    }
}

#[test]
fn installs_shows_an_update_when_the_repo_moves_on_updates_in_place_removes() {
    let (_root, dir) = themes_dir();
    let f = fake(files(vec![(
        COMMUNITY_THEMES_URL.to_owned(),
        json!({ "themes": [{ "repo": "ana/ember" }] })
            .to_string()
            .into_bytes(),
    )]));
    let now = Arc::new(AtomicI64::new(0));
    let mut c = community(&f, &dir, Some(now.clone()));
    assert_eq!(c.install("ana/ember").unwrap(), Ok("ember".into()));
    let listed = list_themes(&dir);
    let t = &listed.themes[0];
    assert_eq!(t.id, "ember");
    assert_eq!(t.author.as_deref(), Some("ana"));
    assert_eq!(t.style.as_ref().unwrap().radius, Some(ThemeRadius::Large));
    let row = &c.list().unwrap().themes[0];
    assert_eq!(row.installed_id.as_deref(), Some("ember"));
    assert!(!row.update);

    {
        let mut all = f.files.lock().unwrap();
        all.insert(
            raw("ana/ember", "manifest.json"),
            manifest(json!({ "version": "1.1.0" })).into_bytes(),
        );
        all.insert(
            raw("ana/ember", "wraithgrid-theme.json"),
            theme(json!({ "colors": { "bg": "#000000", "text": "#ffffff", "accent": "#ff0000" } }))
                .into_bytes(),
        );
    }
    // Still the kept list for five minutes, then read again.
    assert_eq!(c.list().unwrap().themes[0].version, "1.0.0");
    now.store(10 * 60_000, Ordering::SeqCst);
    let row = &c.list().unwrap().themes[0];
    assert!(row.update);
    assert_eq!(row.version, "1.1.0");
    assert_eq!(row.installed_version.as_deref(), Some("1.0.0"));
    assert_eq!(c.install("ana/ember").unwrap(), Ok("ember".into()));
    assert_eq!(list_themes(&dir).themes[0].colors.accent, "#ff0000");
    let row = &c.list().unwrap().themes[0];
    assert!(!row.update);
    assert_eq!(row.installed_version.as_deref(), Some("1.1.0"));

    assert!(c.uninstall("ember").unwrap());
    assert!(list_themes(&dir).themes.is_empty());
    assert!(!c.uninstall("ember").unwrap());
    // The lock file is not a theme file.
    assert!(list_themes(&dir).problems.is_empty());
}

#[test]
fn the_installed_file_and_lock_read_as_the_electron_app_writes_them() {
    let (_root, dir) = themes_dir();
    let c = community(&fake(files(vec![])), &dir, None);
    c.install("ana/ember").unwrap().unwrap();
    // `{ ...theme, author }`: the manifest's author goes last.
    assert_eq!(
        fs::read_to_string(dir.join("ember.json")).unwrap(),
        r##"{
  "name": "Ember",
  "mode": "dark",
  "colors": {
    "bg": "#1a1210",
    "text": "#f5e9e1",
    "accent": "#ff7a45"
  },
  "style": {
    "radius": "large"
  },
  "author": "ana"
}
"##
    );
    assert_eq!(
        fs::read_to_string(dir.join("community.lock")).unwrap(),
        "{\n  \"ember\": {\n    \"repo\": \"ana/ember\",\n    \"version\": \"1.0.0\"\n  }\n}\n"
    );
}

#[test]
fn a_bad_or_missing_theme_file_says_why_and_installs_nothing() {
    let cases: [(Option<String>, &str); 3] = [
        (
            Some(theme(json!({ "style": { "radius": "huge" } }))),
            "ana/ember: wraithgrid-theme.json — style.radius: Invalid option: expected one of \"small\"|\"medium\"|\"large\". Nothing was installed.",
        ),
        (
            Some(theme(json!({ "mode": "light" }))),
            "ana/ember: the theme is light but its manifest says dark.",
        ),
        (
            None,
            "ana/ember: wraithgrid-theme.json is missing, too large or not JSON.",
        ),
    ];
    for (body, why) in cases {
        let (_root, dir) = themes_dir();
        let mut all = files(vec![]);
        match body {
            None => {
                all.remove(&raw("ana/ember", "wraithgrid-theme.json"));
            }
            Some(b) => {
                all.insert(raw("ana/ember", "wraithgrid-theme.json"), b.into_bytes());
            }
        }
        let c = community(&fake(all), &dir, None);
        assert_eq!(c.install("ana/ember").unwrap(), Err(why.to_owned()));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    }
}

#[test]
fn refuses_a_theme_that_needs_a_newer_app_and_odd_repo_names() {
    let (_root, dir) = themes_dir();
    let all = files(vec![(
        raw("ana/ember", "manifest.json"),
        manifest(json!({ "minAppVersion": "9.0.0" })).into_bytes(),
    )]);
    let c = community(&fake(all), &dir, None);
    assert_eq!(
        c.install("ana/ember").unwrap(),
        Err("Ember needs Wraithgrid 9.0.0 or newer (this is 1.15.0).".into())
    );
    assert_eq!(
        c.install("../etc").unwrap(),
        Err("Not a GitHub repository name.".into())
    );
    assert_eq!(
        c.install("ana/nothing").unwrap(),
        Err("ana/nothing has no valid manifest.json.".into())
    );
}

#[test]
fn a_screenshot_is_shown_only_if_it_is_a_png() {
    let (_root, dir) = themes_dir();
    let c = community(&fake(files(vec![])), &dir, None);
    assert!(c
        .screenshot("ana/ember")
        .unwrap()
        .starts_with("data:image/png;base64,iVBORw0KGgo"));
    let bad = community(
        &fake(files(vec![(
            raw("ana/ember", "screenshot.png"),
            b"<svg onload=x>".to_vec(),
        )])),
        &dir,
        None,
    );
    assert_eq!(bad.screenshot("ana/ember"), None);
}

#[test]
fn too_large_answers_are_refused_by_their_length() {
    let (_root, dir) = themes_dir();
    let mut big = manifest(json!({}));
    big.push_str(&" ".repeat(20_000));
    let all = files(vec![(raw("ana/ember", "manifest.json"), big.into_bytes())]);
    let c = community(&fake(all), &dir, None);
    assert_eq!(c.manifest("ana/ember"), None);
}

#[test]
fn reads_a_repository_from_its_address() {
    assert_eq!(
        repo_from_url("https://github.com/ana/ember").as_deref(),
        Some("ana/ember")
    );
    assert_eq!(
        repo_from_url("https://github.com/ana/ember.git").as_deref(),
        Some("ana/ember")
    );
    assert_eq!(
        repo_from_url("https://github.com/ana/ember/tree/main").as_deref(),
        Some("ana/ember")
    );
    assert_eq!(repo_from_url("ana/ember").as_deref(), Some("ana/ember"));
    assert_eq!(repo_from_url("https://evil.example/ana/ember"), None);
    assert_eq!(repo_from_url("http://github.com/ana/ember"), None);
    assert!(newer_version("1.10.0", "1.9.3"));
    assert!(!newer_version("1.0.0", "1.0.0"));
    assert!(!newer_version("1.9.3", "1.10.0"));
}
