//! Community themes (#24, src/main/community-themes.ts): a list of GitHub repositories
//! (cachewraith/wraithgrid-themes), each holding `manifest.json`, `wraithgrid-theme.json` and
//! `screenshot.png`. Everything is read from raw.githubusercontent.com over https with size
//! limits, and nothing is trusted: the repository names, the manifest and the theme
//! (parse_theme_file) are checked, and a theme is data only. What was installed from where
//! is kept in `community.lock` beside the themes.
//!
//! Network access is a `FetchFn` the caller passes in ([`ureq_fetch`] for real), so tests
//! use a fake GitHub. Calls block; run them off the UI thread.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::prefs::ThemeName;
use crate::store::{is_built_in, pretty, valid_id, write_value_as};
use crate::theme::{theme_slug, THEMES_FILE_MAX};
use crate::validate::{
    at, js_trim, object, one_of, parse_theme_file, plain_text, string, trimmed, Check, Issue,
};

/// Fixed on purpose: the app never fetches an address that came from config or the UI.
pub const COMMUNITY_THEMES_URL: &str =
    "https://raw.githubusercontent.com/cachewraith/wraithgrid-themes/main/community-themes.json";

fn raw(repo: &str, file: &str) -> String {
    format!("https://raw.githubusercontent.com/{repo}/HEAD/{file}")
}

const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_LIST_BYTES: usize = 200_000;
const MAX_MANIFEST_BYTES: usize = 20_000;
const MAX_SHOT_BYTES: usize = 1_500_000;
const MAX_REPOS: usize = 200;
const PARALLEL: usize = 6;
const KEEP_MS: i64 = 5 * 60_000;
const LOCK: &str = "community.lock";

/// `owner/repo`, GitHub's own character rules.
static THEME_REPO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})/[A-Za-z0-9._-]{1,100}$").expect("repo regex")
});
static VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]{1,4}\.[0-9]{1,4}\.[0-9]{1,4}$").expect("version regex"));
/// A github.com address; `.` stops at line ends as in JS.
static REPO_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^https://github\.com/([^/\s?#]+/[^/\s?#]+?)(?:\.git)?(?:[/?#][^\n\r\x{2028}\x{2029}]*)?$",
    )
    .expect("url regex")
});

fn is_repo(s: &str) -> bool {
    THEME_REPO.is_match(s)
}

fn is_version(s: &str) -> bool {
    VERSION.is_match(s)
}

/// A GET the library wants made.
#[derive(Clone, Debug)]
pub struct FetchRequest<'a> {
    pub url: &'a str,
    pub user_agent: &'a str,
    pub timeout: Duration,
    /// The most bytes the caller will use; a fetcher may stop reading after this many
    /// (JSON is measured in UTF-16 units afterwards, so `3 * max_bytes` bytes always do).
    pub max_bytes: usize,
}

/// Any answer, whatever its status. No answer at all is the fetcher's `Err`.
#[derive(Clone, Debug, Default)]
pub struct FetchResponse {
    pub status: u16,
    /// The `content-length` header, when there is one.
    pub content_length: Option<u64>,
    pub body: Vec<u8>,
}

pub type FetchFn = Box<dyn Fn(&FetchRequest<'_>) -> anyhow::Result<FetchResponse> + Send + Sync>;

/// A blocking GET with ureq (rustls). Bodies longer than the request allows are an error.
pub fn ureq_fetch(req: &FetchRequest<'_>) -> anyhow::Result<FetchResponse> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(req.timeout))
        .http_status_as_error(false)
        .build()
        .into();
    let mut res = agent
        .get(req.url)
        .header("User-Agent", req.user_agent)
        .call()?;
    let status = res.status().as_u16();
    let content_length = res
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok());
    let limit = req.max_bytes.saturating_mul(3).max(1) as u64;
    let body = res.body_mut().with_config().limit(limit).read_to_vec()?;
    Ok(FetchResponse {
        status,
        content_length,
        body,
    })
}

/// `1.10.0` > `1.9.3`.
pub fn newer_version(a: &str, b: &str) -> bool {
    // `Number()` of each part: blank is 0, junk NaN; a missing part is `undefined`.
    fn parts(s: &str) -> Vec<f64> {
        s.split('.')
            .map(|p| {
                let p = js_trim(p);
                if p.is_empty() {
                    0.0
                } else {
                    p.parse().unwrap_or(f64::NAN)
                }
            })
            .collect()
    }
    let (x, y) = (parts(a), parts(b));
    for i in 0..3 {
        let (xi, yi) = (x.get(i).copied(), y.get(i).copied());
        let same = match (xi, yi) {
            (Some(p), Some(q)) => p == q,
            (None, None) => true,
            _ => false,
        };
        if !same {
            return xi.unwrap_or(0.0) > yi.unwrap_or(0.0);
        }
    }
    false
}

/// `https://github.com/owner/repo` (with or without a path after it) → `owner/repo`.
pub fn repo_from_url(url: &str) -> Option<String> {
    let url = js_trim(url);
    let repo = match REPO_URL.captures(url) {
        Some(m) => m.get(1).map(|g| g.as_str()),
        None => is_repo(url).then_some(url),
    }?;
    is_repo(repo).then(|| repo.to_owned())
}

/// A repository's `manifest.json`, checked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeManifest {
    pub name: String,
    pub author: String,
    pub version: String,
    pub min_app_version: Option<String>,
    pub description: String,
    pub modes: Vec<ThemeName>,
}

fn text(v: Option<&Value>, min: usize, max: usize) -> Check<String> {
    let s = trimmed(v, min, max)?;
    if plain_text(&s) {
        Ok(s)
    } else {
        Err(Issue {
            path: Vec::new(),
            message: "bad text".into(),
        })
    }
}

fn version(v: Option<&Value>) -> Check<String> {
    let s = string(v)?;
    if is_version(s) {
        Ok(s.to_owned())
    } else {
        Err(Issue {
            path: Vec::new(),
            message: "bad version".into(),
        })
    }
}

/// The TS `manifestSchema`; only pass or fail matters (a bad manifest is left out).
fn parse_manifest(v: &Value) -> Option<ThemeManifest> {
    let check = || -> Check<ThemeManifest> {
        let o = object(Some(v))?;
        let modes = match o.get("modes") {
            Some(Value::Array(a)) if (1..=2).contains(&a.len()) => a
                .iter()
                .map(|m| one_of(Some(m), ThemeName::ALL, ThemeName::as_str))
                .collect::<Check<Vec<_>>>()?,
            _ => {
                return at(
                    "modes",
                    Err(Issue {
                        path: vec![],
                        message: "bad modes".into(),
                    }),
                )
            }
        };
        Ok(ThemeManifest {
            name: text(o.get("name"), 1, 40)?,
            author: text(o.get("author"), 1, 60)?,
            version: version(o.get("version"))?,
            min_app_version: match o.get("minAppVersion") {
                None => None,
                v => Some(version(v)?),
            },
            description: match o.get("description") {
                None => String::new(),
                v => text(v, 0, 300)?,
            },
            modes,
        })
    };
    check().ok()
}

/// A gallery row: the manifest, and what is installed from the repository.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityTheme {
    /// `owner/repo` on GitHub.
    pub repo: String,
    pub name: String,
    pub author: String,
    pub version: String,
    pub min_app_version: Option<String>,
    pub description: String,
    pub modes: Vec<ThemeName>,
    /// Installed as this theme id, at this version; None when not installed.
    pub installed_id: Option<String>,
    pub installed_version: Option<String>,
    pub update: bool,
    /// Needs a newer Wraithgrid than this one.
    pub too_new: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CommunityThemeList {
    pub themes: Vec<CommunityTheme>,
}

/// `community.lock`: theme id → where it came from, in the file's order.
#[derive(Clone, Debug, Default)]
struct Lock(Vec<(String, LockEntry)>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct LockEntry {
    repo: String,
    version: String,
}

impl Lock {
    fn has(&self, id: &str) -> bool {
        self.0.iter().any(|(k, _)| k == id)
    }
    fn id_of(&self, repo: &str) -> Option<&(String, LockEntry)> {
        self.0.iter().find(|(_, e)| e.repo == repo)
    }
    fn set(&mut self, id: &str, entry: LockEntry) {
        match self.0.iter_mut().find(|(k, _)| k == id) {
            Some((_, e)) => *e = entry,
            None => self.0.push((id.to_owned(), entry)),
        }
    }
    fn to_json(&self) -> Value {
        let mut m = Map::new();
        for (id, e) in &self.0 {
            let mut o = Map::new();
            o.insert("repo".into(), Value::String(e.repo.clone()));
            o.insert("version".into(), Value::String(e.version.clone()));
            m.insert(id.clone(), Value::Object(o));
        }
        Value::Object(m)
    }
}

pub struct CommunityThemesDeps {
    pub fetch: FetchFn,
    /// The themes folder (~/.wraithgrid/themes).
    pub dir: PathBuf,
    /// This app's version, for a manifest's `minAppVersion`.
    pub app_version: String,
    /// Milliseconds; the gallery list is kept for five minutes.
    pub now: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl CommunityThemesDeps {
    /// The real network (ureq) and clock.
    pub fn live(dir: PathBuf, app_version: impl Into<String>) -> Self {
        Self {
            fetch: Box::new(ureq_fetch),
            dir,
            app_version: app_version.into(),
            now: Box::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as i64)
            }),
        }
    }
}

struct Entry {
    repo: String,
    manifest: ThemeManifest,
}

pub struct CommunityThemes {
    deps: CommunityThemesDeps,
    last: Option<(i64, Vec<Entry>)>,
}

impl CommunityThemes {
    pub fn new(deps: CommunityThemesDeps) -> Self {
        Self { deps, last: None }
    }

    fn dir(&self) -> &Path {
        &self.deps.dir
    }

    /// A 2xx answer within `max` bytes by its content-length; None otherwise.
    fn get(&self, url: &str, max: usize) -> Option<FetchResponse> {
        let res = (self.deps.fetch)(&FetchRequest {
            url,
            user_agent: "Wraithgrid",
            timeout: TIMEOUT,
            max_bytes: max,
        })
        .ok()?;
        if !(200..300).contains(&res.status) {
            return None;
        }
        if res.content_length.unwrap_or(0) > max as u64 {
            return None;
        }
        Some(res)
    }

    /// JSON at `url`, at most `max` characters; None when missing, too large or bad.
    fn json(&self, url: &str, max: usize) -> Option<Value> {
        let res = self.get(url, max)?;
        let text = String::from_utf8_lossy(&res.body);
        if text.encode_utf16().count() > max {
            return None;
        }
        serde_json::from_str(&text).ok()
    }

    fn lock(&self) -> Lock {
        let read = || -> Option<Lock> {
            let text = fs::read(self.dir().join(LOCK)).ok()?;
            let v: Value = serde_json::from_str(&String::from_utf8_lossy(&text)).ok()?;
            let mut lock = Lock::default();
            for (id, e) in v.as_object()? {
                let e = e.as_object()?;
                let repo = e.get("repo")?.as_str().filter(|r| is_repo(r))?;
                let version = e.get("version")?.as_str().filter(|v| is_version(v))?;
                lock.0.push((
                    id.clone(),
                    LockEntry {
                        repo: repo.into(),
                        version: version.into(),
                    },
                ));
            }
            Some(lock)
        };
        read().unwrap_or_default()
    }

    fn save_lock(&self, lock: &Lock) -> anyhow::Result<()> {
        fs::write(self.dir().join(LOCK), pretty(&lock.to_json()))?;
        Ok(())
    }

    /// A repository's manifest, checked; None when missing or bad.
    pub fn manifest(&self, repo: &str) -> Option<ThemeManifest> {
        if !is_repo(repo) {
            return None;
        }
        parse_manifest(&self.json(&raw(repo, "manifest.json"), MAX_MANIFEST_BYTES)?)
    }

    /// The gallery: every listed theme with its manifest, and what is installed.
    pub fn list(&mut self) -> anyhow::Result<CommunityThemeList> {
        let fresh = matches!(&self.last, Some((at, _)) if (self.deps.now)() - at < KEEP_MS);
        if !fresh {
            let repos = self
                .json(COMMUNITY_THEMES_URL, MAX_LIST_BYTES)
                .and_then(|d| parse_list(&d))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Could not read the community theme list. Check your connection."
                    )
                })?;
            let mut found = Vec::new();
            for part in repos.chunks(PARALLEL) {
                let this = &*self;
                let got: Vec<Option<ThemeManifest>> = std::thread::scope(|s| {
                    let jobs: Vec<_> = part
                        .iter()
                        .map(|repo| s.spawn(move || this.manifest(repo)))
                        .collect();
                    jobs.into_iter().map(|j| j.join().ok().flatten()).collect()
                });
                for (repo, m) in part.iter().zip(got) {
                    if let Some(manifest) = m {
                        found.push(Entry {
                            repo: repo.clone(),
                            manifest,
                        });
                    }
                }
            }
            self.last = Some(((self.deps.now)(), found));
        }
        let lock = self.lock();
        let list = self.last.as_ref().map_or(&[][..], |(_, l)| l.as_slice());
        Ok(CommunityThemeList {
            themes: list.iter().map(|e| self.row(e, &lock)).collect(),
        })
    }

    fn row(&self, e: &Entry, lock: &Lock) -> CommunityTheme {
        let installed = lock.id_of(&e.repo);
        let m = &e.manifest;
        CommunityTheme {
            repo: e.repo.clone(),
            name: m.name.clone(),
            author: m.author.clone(),
            version: m.version.clone(),
            min_app_version: m.min_app_version.clone(),
            description: m.description.clone(),
            modes: m.modes.clone(),
            installed_id: installed.map(|(id, _)| id.clone()),
            installed_version: installed.map(|(_, l)| l.version.clone()),
            update: installed.is_some_and(|(_, l)| newer_version(&m.version, &l.version)),
            too_new: m
                .min_app_version
                .as_deref()
                .is_some_and(|v| newer_version(v, &self.deps.app_version)),
        }
    }

    /// The repository's screenshot as a data URL (shown only in the gallery), or None.
    pub fn screenshot(&self, repo: &str) -> Option<String> {
        if !is_repo(repo) {
            return None;
        }
        let buf = self.get(&raw(repo, "screenshot.png"), MAX_SHOT_BYTES)?.body;
        // Only a PNG, by its signature: anything else is not shown.
        const PNG: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        let png = buf.len() > 8 && buf[..8] == PNG;
        (png && buf.len() <= MAX_SHOT_BYTES)
            .then(|| format!("data:image/png;base64,{}", base64(&buf)))
    }

    /// Installs (or updates) the repository's theme: `Ok(id)`, or `Err(why)` for the UI.
    /// The outer error is a failed write.
    pub fn install(&self, repo: &str) -> anyhow::Result<Result<String, String>> {
        if !is_repo(repo) {
            return Ok(Err("Not a GitHub repository name.".into()));
        }
        let Some(m) = self.manifest(repo) else {
            return Ok(Err(format!("{repo} has no valid manifest.json.")));
        };
        if let Some(min) = &m.min_app_version {
            if newer_version(min, &self.deps.app_version) {
                return Ok(Err(format!(
                    "{} needs Wraithgrid {min} or newer (this is {}).",
                    m.name, self.deps.app_version
                )));
            }
        }
        let Some(data) = self.json(&raw(repo, "wraithgrid-theme.json"), THEMES_FILE_MAX) else {
            return Ok(Err(format!(
                "{repo}: wraithgrid-theme.json is missing, too large or not JSON."
            )));
        };
        let theme = match parse_theme_file(&data) {
            Ok(t) => t,
            Err(e) => {
                return Ok(Err(format!(
                    "{repo}: wraithgrid-theme.json — {e}. Nothing was installed."
                )))
            }
        };
        if !m.modes.contains(&theme.mode) {
            let modes: Vec<&str> = m.modes.iter().map(|x| x.as_str()).collect();
            return Ok(Err(format!(
                "{repo}: the theme is {} but its manifest says {}.",
                theme.mode,
                modes.join(" / ")
            )));
        }
        // The manifest's author when the theme names none; added last, as `{ ...theme,
        // author }` does in the TS, so the file reads the same.
        let mut value = serde_json::to_value(&theme)?;
        if theme.author.is_none() {
            if let Value::Object(o) = &mut value {
                o.insert("author".into(), Value::String(m.author.clone()));
            }
        }
        let mut lock = self.lock();
        let id = match lock.id_of(repo) {
            Some((id, _)) => id.clone(),
            None => self.free_id(&theme.name, &lock),
        };
        write_value_as(&value, self.dir(), &id)?;
        lock.set(
            &id,
            LockEntry {
                repo: repo.into(),
                version: m.version,
            },
        );
        self.save_lock(&lock)?;
        Ok(Ok(id))
    }

    fn free_id(&self, name: &str, lock: &Lock) -> String {
        let slug: String = theme_slug(name).chars().take(36).collect();
        let taken = |id: &str| {
            is_built_in(id)
                || lock.has(id)
                || fs::File::open(self.dir().join(format!("{id}.json")))
                    .and_then(|f| f.metadata())
                    .is_ok_and(|m| m.is_file())
        };
        let mut id = slug.clone();
        let mut i = 2;
        while taken(&id) {
            id = format!("{slug}-{i}");
            i += 1;
        }
        id
    }

    /// Removes an installed community theme (its file and its line in the lock).
    pub fn uninstall(&self, id: &str) -> anyhow::Result<bool> {
        let mut lock = self.lock();
        if !lock.has(id) || !valid_id(id) {
            return Ok(false);
        }
        lock.0.retain(|(k, _)| k != id);
        let _ = fs::remove_file(self.dir().join(format!("{id}.json")));
        self.save_lock(&lock)?;
        Ok(true)
    }
}

/// The TS `listSchema`: `{ themes: [{ repo }] }`, at most 200, every repo valid; the repos
/// without repeats.
fn parse_list(v: &Value) -> Option<Vec<String>> {
    let themes = v.as_object()?.get("themes")?.as_array()?;
    if themes.len() > MAX_REPOS {
        return None;
    }
    let mut repos: Vec<String> = Vec::new();
    for t in themes {
        let repo = t.as_object()?.get("repo")?.as_str()?;
        if !is_repo(repo) {
            return None;
        }
        if !repos.iter().any(|r| r == repo) {
            repos.push(repo.to_owned());
        }
    }
    Some(repos)
}

/// Standard base64 with padding.
fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
