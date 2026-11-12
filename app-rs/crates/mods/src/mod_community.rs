//! claudemod.com is a community list of mods, each with its GitHub repository (port of
//! `src/main/mod-community.ts`). The site has no API: the list is read from its browse page,
//! and what can be installed is read from each repository's own marketplace file; a
//! repository that is a bare plugin (only `.claude-plugin/plugin.json`) is offered for
//! download into a mod folder instead. Nothing here is trusted: every repository,
//! marketplace and mod name is checked before it can reach a `claude plugin` command.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use crate::mods::{
    count_of, description, nullish_str, str_in, CatalogMod, Marketplace, GITHUB_REPO, MOD_ID,
};
use crate::util::js_len;

/// Fixed on purpose: the app never fetches an address that came from config or the UI.
pub const COMMUNITY_URL: &str = "https://www.claudemod.com/browse?category=mod";
const TIMEOUT: Duration = Duration::from_millis(10_000);
const MAX_PAGE_BYTES: usize = 2_000_000;
const MAX_MANIFEST_BYTES: usize = 500_000;
const MAX_REPOS: usize = 60;
const PARALLEL: usize = 6;
/// A repository's marketplace can list many mods; past this many it is a plugin store.
const MODS_PER_REPO: usize = 20;
/// Opening the list twice in a row reuses the answer.
const KEEP_MS: i64 = 5 * 60_000;

/// One GET: the address, its headers and how long it may take.
#[derive(Clone, Copy, Debug)]
pub struct FetchRequest<'a> {
    pub url: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    pub timeout: Duration,
}

/// Any answer, whatever its status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchResponse {
    pub status: u16,
    pub body: String,
}

/// A GET; Err only when no answer came at all (offline, timeout, TLS).
pub trait Fetch: Send + Sync {
    fn fetch(&self, req: &FetchRequest<'_>) -> Result<FetchResponse, String>;
}

impl<F> Fetch for F
where
    F: Fn(&FetchRequest<'_>) -> Result<FetchResponse, String> + Send + Sync,
{
    fn fetch(&self, req: &FetchRequest<'_>) -> Result<FetchResponse, String> {
        self(req)
    }
}

/// The real network: a blocking GET with ureq. Any status comes back as a response.
pub fn ureq_fetch(req: &FetchRequest<'_>) -> Result<FetchResponse, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(req.timeout))
        .http_status_as_error(false)
        .build()
        .into();
    let mut r = agent.get(req.url);
    for (k, v) in req.headers {
        r = r.header(*k, *v);
    }
    let mut res = r.call().map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    // Bodies past the caps are refused by length anyway; UTF-8 is read as `res.text()` does.
    let body = res
        .body_mut()
        .with_config()
        .limit(4 * MAX_PAGE_BYTES as u64)
        .lossy_utf8(true)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    Ok(FetchResponse { status, body })
}

/// One entry of the site's list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CommunityEntry {
    pub slug: String,
    /// `owner/repo` on GitHub.
    pub repo: String,
    pub stars: Option<u64>,
}

static GITHUB_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https://github\.com/([^/]+/[^/]+?)(?:\.git)?/?$").expect("a valid pattern")
});
/// A marketplace or mod name on its own: neither may read as an option or hold an `@`.
static NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_][A-Za-z0-9_.-]*$").expect("a valid pattern"));

/// What `/\{[^{}]{0,600}"githubUrl":"[^{}]{0,600}\}/g` matches in `text`, in order. A match
/// spans from a `{` to the next brace, which must be a `}`, with the key no more than 600
/// characters (UTF-16 units) from either end.
fn github_objects(text: &str) -> Vec<&str> {
    const KEY: &str = "\"githubUrl\":\"";
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(open) = text[from..].find('{').map(|i| from + i) {
        let Some(close) = text[open + 1..].find(['{', '}']).map(|i| open + 1 + i) else {
            break;
        };
        from = close;
        if text.as_bytes()[close] != b'}' {
            continue;
        }
        let inner = &text[open + 1..close];
        let fits = inner
            .match_indices(KEY)
            .any(|(at, _)| js_len(&inner[..at]) <= 600 && js_len(&inner[at + KEY.len()..]) <= 600);
        if fits {
            out.push(&text[open..=close]);
            from = close + 1;
        }
    }
    out
}

/// The mods on the browse page: the objects in its data that name a GitHub repository.
pub fn parse_community_page(html: &str) -> Vec<CommunityEntry> {
    // The page carries its data as JSON inside a script string, quotes escaped.
    let text = html.replace("\\\"", "\"");
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in github_objects(&text) {
        let Ok(Value::Object(o)) = serde_json::from_str::<Value>(m) else {
            continue;
        };
        let (Some(slug), Some(url)) = (
            str_in(o.get("slug"), 1, 120),
            str_in(o.get("githubUrl"), 0, 300),
        ) else {
            continue;
        };
        let Some(repo) = GITHUB_URL.captures(url).and_then(|c| c.get(1)) else {
            continue;
        };
        if !GITHUB_REPO.is_match(repo.as_str()) || !seen.insert(slug.to_string()) {
            continue;
        }
        out.push(CommunityEntry {
            slug: slug.to_string(),
            repo: repo.as_str().to_string(),
            stars: count_of(o.get("githubStars")),
        });
    }
    out.truncate(MAX_REPOS);
    out
}

/// A plugin of a marketplace file, as zod's `manifest` schema reads it.
struct Plugin<'a> {
    name: &'a str,
    description: Option<&'a Value>,
    version: Option<String>,
}

fn plugin_entry(v: &Value) -> Option<Plugin<'_>> {
    let o = v.as_object()?;
    Some(Plugin {
        name: str_in(o.get("name"), 1, 120)?,
        description: o.get("description"),
        version: nullish_str(o.get("version"), 64),
    })
}

/// The catalog rows for a site entry, from its repository's `.claude-plugin/marketplace.json`:
/// every mod of that marketplace, the one named like the entry first. None when the
/// repository offers nothing `claude plugin install` could take.
pub fn community_mod(
    entry: &CommunityEntry,
    manifest: &Value,
) -> Option<(Vec<CatalogMod>, Marketplace)> {
    let o = manifest.as_object()?;
    let market = str_in(o.get("name"), 1, 120)?;
    let list = o.get("plugins")?.as_array()?;
    if list.len() > 500 {
        return None;
    }
    // One plugin that is not one makes the whole file unreadable, as the schema does.
    let mut plugins = list.iter().map(plugin_entry).collect::<Option<Vec<_>>>()?;
    if !NAME.is_match(market) {
        return None;
    }
    let url = format!("https://github.com/{}", entry.repo);
    plugins.sort_by_key(|p| p.name != entry.slug);
    let mods: Vec<CatalogMod> = plugins
        .into_iter()
        .filter(|p| NAME.is_match(p.name) && MOD_ID.is_match(&format!("{}@{market}", p.name)))
        .take(MODS_PER_REPO)
        .map(|p| CatalogMod {
            id: format!("{}@{market}", p.name),
            name: p.name.to_string(),
            description: description(p.description),
            marketplace: market.to_string(),
            version: p.version,
            installs: None,
            stars: entry.stars,
            clone: None,
            url: Some(url.clone()),
        })
        .collect();
    if mods.is_empty() {
        return None;
    }
    let marketplace = Marketplace {
        name: market.to_string(),
        url: Some(url),
        add: Some(entry.repo.clone()),
    };
    Some((mods, marketplace))
}

/// The catalog row for a site entry whose repository is one bare plugin: downloaded into a
/// mod folder, since there is no marketplace for `claude plugin install` to take it from.
pub fn community_plugin(entry: &CommunityEntry, plugin: &Value) -> Option<CatalogMod> {
    let p = plugin_entry(plugin)?;
    if !NAME.is_match(p.name) {
        return None;
    }
    Some(CatalogMod {
        // Not an install id: `@folder` marks a mod that lives in a mod folder.
        id: format!("{}@folder", p.name),
        name: p.name.to_string(),
        description: description(p.description),
        marketplace: "mod folder".into(),
        version: p.version,
        installs: None,
        stars: entry.stars,
        clone: Some(entry.repo.clone()),
        url: Some(format!("https://github.com/{}", entry.repo)),
    })
}

/// The community list: its mods, and where each mod's marketplace is added from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CommunityMods {
    pub mods: Vec<CatalogMod>,
    pub marketplaces: Vec<Marketplace>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Reads the site's list, then each listed repository's marketplace file.
pub struct CommunityCatalog {
    fetch: Box<dyn Fetch>,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
    last: Mutex<Option<(i64, CommunityMods)>>,
}

impl CommunityCatalog {
    /// With the real network and clock.
    pub fn live() -> Self {
        Self::new(ureq_fetch)
    }

    pub fn new(fetch: impl Fetch + 'static) -> Self {
        Self::with_clock(fetch, now_ms)
    }

    /// `now` in milliseconds.
    pub fn with_clock(
        fetch: impl Fetch + 'static,
        now: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        CommunityCatalog {
            fetch: Box::new(fetch),
            now: Box::new(now),
            last: Mutex::new(None),
        }
    }

    /// The body of a 2xx answer within `max_bytes` (UTF-16 units), else None.
    fn fetch_text(&self, url: &str, max_bytes: usize) -> Result<Option<String>, String> {
        let res = self.fetch.fetch(&FetchRequest {
            url,
            headers: &[("User-Agent", "Wraithgrid")],
            timeout: TIMEOUT,
        })?;
        if !(200..300).contains(&res.status) {
            return Ok(None);
        }
        Ok((js_len(&res.body) <= max_bytes).then_some(res.body))
    }

    /// One site entry's mods, and its marketplace when it has one.
    fn read_entry(&self, entry: &CommunityEntry) -> Option<(Vec<CatalogMod>, Option<Marketplace>)> {
        let raw = |file: &str| {
            let url = format!(
                "https://raw.githubusercontent.com/{}/HEAD/.claude-plugin/{file}",
                entry.repo
            );
            self.fetch_text(&url, MAX_MANIFEST_BYTES).ok().flatten()
        };
        if let Some(text) = raw("marketplace.json") {
            let json: Value = serde_json::from_str(&text).ok()?;
            return community_mod(entry, &json).map(|(mods, m)| (mods, Some(m)));
        }
        let json: Value = serde_json::from_str(&raw("plugin.json")?).ok()?;
        Some((vec![community_plugin(entry, &json)?], None))
    }

    pub fn read(&self) -> Result<CommunityMods, String> {
        if let Some((at, value)) = &*self.last.lock().unwrap_or_else(|e| e.into_inner()) {
            if (self.now)() - at < KEEP_MS {
                return Ok(value.clone());
            }
        }
        let page = self
            .fetch_text(COMMUNITY_URL, MAX_PAGE_BYTES)
            .ok()
            .flatten();
        let Some(page) = page else {
            return Err("Could not reach claudemod.com. Check your connection.".into());
        };
        let entries = parse_community_page(&page);
        if entries.is_empty() {
            return Err("claudemod.com listed no mods Wraithgrid can read.".into());
        }
        let mut found = Vec::new();
        for batch in entries.chunks(PARALLEL) {
            thread::scope(|s| {
                let running: Vec<_> = batch
                    .iter()
                    .map(|entry| s.spawn(|| self.read_entry(entry)))
                    .collect();
                found.extend(running.into_iter().map(|h| h.join().ok().flatten()));
            });
        }
        // Several site entries can share one repository: list its mods and marketplace once.
        let mut value = CommunityMods::default();
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for (mods, marketplace) in found.into_iter().flatten() {
            value
                .mods
                .extend(mods.into_iter().filter(|m| ids.insert(m.id.clone())));
            if let Some(m) = marketplace.filter(|m| names.insert(m.name.clone())) {
                value.marketplaces.push(m);
            }
        }
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(((self.now)(), value.clone()));
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    /// A site card: the site ships its data as JSON inside a script string, quotes escaped.
    fn card(slug: &str, github_url: &str, stars: Value) -> String {
        let data =
            json!({ "modId": "x", "githubStars": stars, "githubUrl": github_url, "slug": slug });
        format!("[\"$\",\"$L37\",null,{data}]").replace('"', "\\\"")
    }

    fn page_of(cards: &[String]) -> String {
        format!(
            "<html><script>self.__next_f.push([1,\"{}\"])</script></html>",
            cards.join(",")
        )
    }

    fn page() -> String {
        page_of(&[
            card(
                "burn-meter",
                "https://github.com/OneWave-AI/claude-code-mods",
                json!(13),
            ),
            card(
                "cctop",
                "https://github.com/tomstagl/cctop.git",
                Value::Null,
            ),
            card("burn-meter", "https://github.com/other/copy", json!(12)),
            card("elsewhere", "https://gitlab.com/a/b", json!(12)),
            card("option", "https://github.com/--upload-pack=x/y", json!(12)),
            card("deep", "https://github.com/a/b/tree/main/c", json!(12)),
        ])
    }

    fn entry() -> CommunityEntry {
        CommunityEntry {
            slug: "burn-meter".into(),
            repo: "OneWave-AI/claude-code-mods".into(),
            stars: Some(13),
        }
    }

    fn ok(body: impl Into<String>) -> Result<FetchResponse, String> {
        Ok(FetchResponse {
            status: 200,
            body: body.into(),
        })
    }

    #[test]
    fn reads_the_github_repositories_the_page_names_once_per_mod() {
        assert_eq!(
            parse_community_page(&page()),
            [
                CommunityEntry {
                    slug: "burn-meter".into(),
                    repo: "OneWave-AI/claude-code-mods".into(),
                    stars: Some(13)
                },
                CommunityEntry {
                    slug: "cctop".into(),
                    repo: "tomstagl/cctop".into(),
                    stars: None
                }
            ]
        );
        assert!(parse_community_page("<html>nothing</html>").is_empty());
    }

    #[test]
    fn matches_objects_as_the_page_pattern_does() {
        let key = "\"githubUrl\":\"";
        let far = format!("{{{}{key}x\"}}", "a".repeat(601));
        let near = format!("{{{}{key}x\"}}", "é".repeat(600));
        assert!(github_objects(&far).is_empty());
        assert_eq!(github_objects(&near), [near.as_str()]);
        assert_eq!(
            github_objects(&format!("{{a{{{key}}}")),
            [format!("{{{key}}}")]
        );
        assert!(github_objects(&format!("{{{key}")).is_empty());
    }

    #[test]
    fn offers_every_mod_of_the_marketplace_the_one_named_like_the_entry_first() {
        let many = json!({
            "name": "claude-code-mods",
            "plugins": [
                { "name": "code-pet", "source": "./code-pet" },
                { "name": "burn-meter", "description": " Live  cost\n odometer ", "version": "1.0.0" },
                { "name": "a b; rm" }
            ]
        });
        let url = "https://github.com/OneWave-AI/claude-code-mods";
        let row = |name: &str, description: &str, version: Option<&str>| CatalogMod {
            id: format!("{name}@claude-code-mods"),
            name: name.into(),
            description: description.into(),
            marketplace: "claude-code-mods".into(),
            version: version.map(Into::into),
            installs: None,
            stars: Some(13),
            clone: None,
            url: Some(url.into()),
        };
        assert_eq!(
            community_mod(&entry(), &many),
            Some((
                vec![
                    row("burn-meter", "Live cost odometer", Some("1.0.0")),
                    row("code-pet", "", None)
                ],
                Marketplace {
                    name: "claude-code-mods".into(),
                    url: Some(url.into()),
                    add: Some("OneWave-AI/claude-code-mods".into())
                }
            ))
        );
        let one = json!({ "name": "flightdeck-market", "plugins": [{ "name": "flightdeck" }] });
        let ids: Vec<_> = community_mod(&entry(), &one)
            .unwrap()
            .0
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, ["flightdeck@flightdeck-market"]);
        let plugins: Vec<_> = (0..30)
            .map(|i| json!({ "name": format!("p{i}") }))
            .collect();
        let store = json!({ "name": "m", "plugins": plugins });
        assert_eq!(community_mod(&entry(), &store).unwrap().0.len(), 20);
    }

    #[test]
    fn offers_nothing_it_could_not_install_safely() {
        for manifest in [
            json!({ "name": "--scope", "plugins": [{ "name": "burn-meter" }] }),
            json!({ "name": "m", "plugins": [{ "name": "a b; rm" }] }),
            json!({ "name": "a@b", "plugins": [{ "name": "burn-meter" }] }),
            json!({ "name": "m", "plugins": [] }),
            json!({ "plugins": [] }),
            json!({ "name": "m", "plugins": [{ "name": "ok" }, { "nope": 1 }] }),
            json!("junk"),
        ] {
            assert_eq!(community_mod(&entry(), &manifest), None, "{manifest}");
        }
    }

    #[test]
    fn offers_a_bare_plugin_repository_for_download_into_a_mod_folder() {
        let plugin =
            json!({ "name": "effort-cycle", "description": " Cycle\n effort ", "version": "2" });
        assert_eq!(
            community_plugin(&entry(), &plugin),
            Some(CatalogMod {
                id: "effort-cycle@folder".into(),
                name: "effort-cycle".into(),
                description: "Cycle effort".into(),
                marketplace: "mod folder".into(),
                version: Some("2".into()),
                installs: None,
                stars: Some(13),
                clone: Some("OneWave-AI/claude-code-mods".into()),
                url: Some("https://github.com/OneWave-AI/claude-code-mods".into())
            })
        );
        assert_eq!(community_plugin(&entry(), &json!({ "name": "--x" })), None);
        assert_eq!(
            community_plugin(&entry(), &json!({ "name": "../up" })),
            None
        );
        assert_eq!(community_plugin(&entry(), &json!("junk")), None);
    }

    #[test]
    fn fetches_the_page_and_each_repository_once_then_reuses_the_answer() {
        let asked = Arc::new(Mutex::new(Vec::<String>::new()));
        let now = Arc::new(AtomicI64::new(0));
        let (log, clock) = (asked.clone(), now.clone());
        let page = page();
        let catalog = CommunityCatalog::with_clock(
            move |req: &FetchRequest<'_>| {
                assert_eq!(req.headers, [("User-Agent", "Wraithgrid")]);
                let url = req.url;
                log.lock().unwrap().push(url.to_string());
                if url == COMMUNITY_URL {
                    return ok(page.clone());
                }
                if url.contains("/tomstagl/cctop/") {
                    return ok(
                        json!({ "name": "cctop", "plugins": [{ "name": "cctop" }] }).to_string()
                    );
                }
                if url.ends_with("/OneWave-AI/claude-code-mods/HEAD/.claude-plugin/plugin.json") {
                    return ok(json!({ "name": "burn-meter" }).to_string());
                }
                Ok(FetchResponse {
                    status: 404,
                    body: "Not Found".into(),
                })
            },
            move || clock.load(Ordering::Relaxed),
        );
        let first = catalog.read().unwrap();
        // The repository without a marketplace file is a bare plugin: offered as a download.
        let rows: Vec<_> = first
            .mods
            .iter()
            .map(|m| (m.id.as_str(), m.clone.as_deref()))
            .collect();
        assert_eq!(
            rows,
            [
                ("burn-meter@folder", Some("OneWave-AI/claude-code-mods")),
                ("cctop@cctop", None)
            ]
        );
        assert_eq!(
            first.marketplaces,
            [Marketplace {
                name: "cctop".into(),
                url: Some("https://github.com/tomstagl/cctop".into()),
                add: Some("tomstagl/cctop".into())
            }]
        );
        let raw = "https://raw.githubusercontent.com";
        let mut got = asked.lock().unwrap().clone();
        got.sort();
        let mut want = vec![
            COMMUNITY_URL.to_string(),
            format!("{raw}/OneWave-AI/claude-code-mods/HEAD/.claude-plugin/marketplace.json"),
            format!("{raw}/OneWave-AI/claude-code-mods/HEAD/.claude-plugin/plugin.json"),
            format!("{raw}/tomstagl/cctop/HEAD/.claude-plugin/marketplace.json"),
        ];
        want.sort();
        assert_eq!(got, want);
        catalog.read().unwrap();
        assert_eq!(asked.lock().unwrap().len(), 4);
        now.store(6 * 60_000, Ordering::Relaxed);
        catalog.read().unwrap();
        assert_eq!(asked.lock().unwrap().len(), 8);
    }

    #[test]
    fn says_so_when_the_site_cannot_be_read() {
        let down = CommunityCatalog::new(|_: &FetchRequest<'_>| Err("offline".to_string()));
        assert_eq!(
            down.read().unwrap_err(),
            "Could not reach claudemod.com. Check your connection."
        );
        let empty = CommunityCatalog::new(|_: &FetchRequest<'_>| ok("<html></html>"));
        assert_eq!(
            empty.read().unwrap_err(),
            "claudemod.com listed no mods Wraithgrid can read."
        );
        let big = "x".repeat(MAX_PAGE_BYTES + 1);
        let huge = CommunityCatalog::new(move |_: &FetchRequest<'_>| ok(big.clone()));
        assert!(huge.read().unwrap_err().starts_with("Could not reach"));
    }

    #[test]
    fn lists_a_repository_two_site_entries_share_once() {
        let shared = page_of(&[
            card(
                "burn-meter",
                "https://github.com/OneWave-AI/claude-code-mods",
                json!(12),
            ),
            card(
                "code-pet",
                "https://github.com/OneWave-AI/claude-code-mods",
                json!(12),
            ),
        ]);
        let market =
            json!({ "name": "claude-code-mods", "plugins": [{ "name": "burn-meter" }, { "name": "code-pet" }] })
                .to_string();
        let catalog = CommunityCatalog::new(move |req: &FetchRequest<'_>| {
            ok(if req.url == COMMUNITY_URL {
                shared.clone()
            } else {
                market.clone()
            })
        });
        let got = catalog.read().unwrap();
        let ids: Vec<_> = got.mods.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            ["burn-meter@claude-code-mods", "code-pet@claude-code-mods"]
        );
        let names: Vec<_> = got.marketplaces.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["claude-code-mods"]);
    }
}
