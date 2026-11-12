//! Mods are claude plugins (port of `src/main/mods.ts` and `src/shared/mods.ts`): installed
//! ones live in an account's config dir. Wraithgrid never reads a config dir itself, so the
//! list and every enable/disable go through the account's own `claude plugin` command, run
//! with that account's CLAUDE_CONFIG_DIR. Everything claude prints is checked before it is
//! shown, and every id is checked before it can reach a command line.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::thread;
use std::time::{Duration, Instant};

use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use crate::util::{encode_uri_component, js_len, js_slice, one_line};

/// The environment a claude run gets, in full (`execFile`'s `env`).
pub type Env = HashMap<String, String>;

/// How one claude run goes.
#[derive(Clone, Copy, Debug)]
pub struct RunOptions<'a> {
    pub env: &'a Env,
    pub cwd: &'a Path,
    pub timeout: Duration,
}

/// Runs the claude binary with these arguments and returns its stdout; Err is the message
/// the UI shows.
pub trait ClaudeRunner: Sync {
    fn run(&self, args: &[String], opts: &RunOptions<'_>) -> Result<String, String>;
}

impl<F> ClaudeRunner for F
where
    F: Fn(&[String], &RunOptions<'_>) -> Result<String, String> + Sync,
{
    fn run(&self, args: &[String], opts: &RunOptions<'_>) -> Result<String, String> {
        self(args, opts)
    }
}

/// A plain claude command.
pub const TIMEOUT: Duration = Duration::from_millis(20_000);
/// The first catalog read of an account fetches its marketplaces.
pub const CATALOG_TIMEOUT: Duration = Duration::from_millis(60_000);
/// An install fetches the mod's repository.
pub const INSTALL_TIMEOUT: Duration = Duration::from_millis(180_000);
const DESCRIPTION_MAX: usize = 400;
/// The scopes `claude plugin enable/disable --scope` takes; others are auto-detected.
const SCOPE_FLAGS: [&str; 3] = ["user", "project", "local"];

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid pattern")
}

/// A plugin id is `name@marketplace`; it must never read as an option.
pub static MOD_ID: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z0-9_][A-Za-z0-9_.@/-]*$"));
/// `owner/repo` on GitHub.
pub static GITHUB_REPO: LazyLock<Regex> =
    LazyLock::new(|| re(r"^[A-Za-z0-9][A-Za-z0-9-]*/[A-Za-z0-9._-]+$"));
/// A marketplace name on its own; it must never read as an option.
pub static MARKETPLACE_NAME: LazyLock<Regex> =
    LazyLock::new(|| re(r"^[A-Za-z0-9_][A-Za-z0-9_.-]{0,119}$"));
/// A git revision safe to put in a page address.
static REV: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z0-9._/-]+$"));
/// A folder Download made: `~/.wraithgrid/mods/<name>`.
static DOWNLOADED: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^~[\\/]\.wraithgrid[\\/]mods[\\/]([A-Za-z0-9_][A-Za-z0-9_.-]{0,119})[\\/]?$")
});

/// One installed mod (a claude plugin), as the account's own `claude plugin list` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModInfo {
    /// `name@marketplace`.
    pub id: String,
    pub version: String,
    /// user, project, local, synced… as claude names it.
    pub scope: String,
    pub enabled: bool,
    /// Set for project and local scopes: the project it is installed for.
    pub project_path: Option<String>,
    /// The page of the marketplace it came from, when that has one.
    pub url: Option<String>,
    /// claude's own remarks, e.g. that another mod of the same name takes precedence.
    pub notes: Vec<String>,
}

/// A mod offered by a marketplace, as an account's `claude plugin list --available` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogMod {
    /// `name@marketplace`: what `claude plugin install` takes.
    pub id: String,
    pub name: String,
    pub description: String,
    pub marketplace: String,
    pub version: Option<String>,
    /// How many installs the marketplace counts, where it says.
    pub installs: Option<u64>,
    /// Its repository's GitHub stars, for a mod from the community list.
    pub stars: Option<u64>,
    /// The mod's https source page, when it has one.
    pub url: Option<String>,
    /// `owner/repo` of a community mod that is a bare plugin (no marketplace file): it is not
    /// installed but downloaded into a mod folder. None for everything `plugin install` takes.
    pub clone: Option<String>,
}

/// A marketplace an account has: where its page is and what `marketplace add` takes for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Marketplace {
    pub name: String,
    /// Its https page, when it has one.
    pub url: Option<String>,
    /// The source to add it to another account with: `owner/repo` or an https address.
    pub add: Option<String>,
}

/// A marketplace merged over the claude accounts: how many of them have it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MarketplaceInfo {
    pub name: String,
    pub url: Option<String>,
    pub accounts: usize,
}

// ---- Mod ids (src/shared/mods.ts) ---------------------------------------------------

/// The marketplace half of a mod id, or '' when it names none.
pub fn marketplace_of(id: &str) -> &str {
    match id.rfind('@') {
        Some(at) if at > 0 => &id[at + 1..],
        _ => "",
    }
}

/// The name half of a mod id.
pub fn mod_name_of(id: &str) -> &str {
    match id.rfind('@') {
        Some(at) if at > 0 => &id[..at],
        _ => id,
    }
}

/// The mod name of a mod folder Wraithgrid downloaded (`~/.wraithgrid/mods/<name>`, as
/// Settings keeps it), or None for a folder of the user's own.
pub fn downloaded_mod_name(dir: &str) -> Option<&str> {
    Some(DOWNLOADED.captures(dir)?.get(1)?.as_str())
}

// ---- What zod checked in the TS -------------------------------------------------------

/// `z.string().min(min).max(max)`: lengths in UTF-16 units, as JavaScript counts them.
pub(crate) fn str_in(v: Option<&Value>, min: usize, max: usize) -> Option<&str> {
    let s = v?.as_str()?;
    (min..=max).contains(&js_len(s)).then_some(s)
}

/// `z.number().int().nonnegative()`: a safe integer, zero or more.
pub(crate) fn count_of(v: Option<&Value>) -> Option<u64> {
    let n = v?.as_f64()?;
    let safe = n.fract() == 0.0 && (0.0..=9_007_199_254_740_991.0).contains(&n);
    safe.then_some(n as u64)
}

/// A `.nullish().catch(null)` field: absent, null or not valid all read as None.
pub(crate) fn nullish_str(v: Option<&Value>, max: usize) -> Option<String> {
    str_in(v, 0, max).map(str::to_string)
}

/// `(s ?? '').replace(/\s+/g, ' ').trim().slice(0, DESCRIPTION_MAX)`.
pub(crate) fn description(v: Option<&Value>) -> String {
    one_line(v.and_then(Value::as_str), DESCRIPTION_MAX)
}

/// `JSON.parse(stdout.slice(stdout.indexOf(open)))`, or `missing` when there is no `open`.
fn json_from(stdout: &str, open: char, missing: &str) -> Result<Value, String> {
    let start = stdout.find(open).ok_or_else(|| missing.to_string())?;
    serde_json::from_str(&stdout[start..]).map_err(|e| e.to_string())
}

// ---- Marketplaces ---------------------------------------------------------------------

/// `raw` as a page to open, only when it is a plain https address.
pub(crate) fn https_page(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    if js_len(raw) > 500 {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let href = url.as_str();
    Some(href.strip_suffix(".git").unwrap_or(href).to_string())
}

/// Reads `claude plugin marketplace list --json`.
pub fn parse_marketplaces(stdout: &str) -> Result<Vec<Marketplace>, String> {
    const NONE: &str = "claude printed no marketplace list";
    let raw = json_from(stdout, '[', NONE)?;
    let list = raw.as_array().ok_or(NONE)?;
    Ok(list
        .iter()
        .filter_map(|item| {
            let o = item.as_object()?;
            let name = str_in(o.get("name"), 1, 120)?.to_string();
            let source = str_in(o.get("source"), 0, 32).unwrap_or("");
            let repo = str_in(o.get("repo"), 0, 200);
            let url = str_in(o.get("url"), 0, 500);
            if let Some(repo) = repo.filter(|r| source == "github" && GITHUB_REPO.is_match(r)) {
                return Some(Marketplace {
                    name,
                    url: Some(format!("https://github.com/{repo}")),
                    add: Some(repo.to_string()),
                });
            }
            let page = https_page(url);
            let add = page.as_ref().and(url).map(str::to_string);
            Some(Marketplace {
                name,
                url: page,
                add,
            })
        })
        .collect())
}

pub fn list_marketplaces(
    run: &dyn ClaudeRunner,
    env: &Env,
    cwd: &Path,
) -> Result<Vec<Marketplace>, String> {
    let args = strings(&["plugin", "marketplace", "list", "--json"]);
    let opts = RunOptions {
        env,
        cwd,
        timeout: TIMEOUT,
    };
    parse_marketplaces(&run.run(&args, &opts)?)
}

/// The arguments that add a marketplace, or None when its source cannot be passed safely.
pub fn marketplace_add_args(source: &str) -> Option<Vec<String>> {
    let ok = GITHUB_REPO.is_match(source) || https_page(Some(source)).is_some();
    ok.then(|| strings(&["plugin", "marketplace", "add", source]))
}

pub fn marketplace_remove_args(name: &str) -> Option<Vec<String>> {
    MARKETPLACE_NAME
        .is_match(name)
        .then(|| strings(&["plugin", "marketplace", "remove", name]))
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

// ---- Installed mods ---------------------------------------------------------------------

/// Reads `claude plugin list --json`. Entries that are not a mod are skipped, never trusted.
pub fn parse_mod_list(stdout: &str) -> Result<Vec<ModInfo>, String> {
    const NONE: &str = "claude printed no mod list";
    let raw = json_from(stdout, '[', NONE)?;
    let list = raw.as_array().ok_or(NONE)?;
    Ok(list
        .iter()
        .filter_map(|item| {
            let o = item.as_object()?;
            let id = str_in(o.get("id"), 1, 200)?;
            // projectPath is optional, but one that is there must be a path.
            let project_path = match o.get("projectPath") {
                None => None,
                Some(v) => Some(str_in(Some(v), 0, 4096)?.to_string()),
            };
            if !MOD_ID.is_match(id) {
                return None;
            }
            let notes = match o.get("notes").and_then(Value::as_array) {
                Some(a) if a.iter().all(Value::is_string) => a
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|n| js_slice(n, 300))
                    .take(4)
                    .collect(),
                _ => Vec::new(),
            };
            Some(ModInfo {
                id: id.to_string(),
                version: str_in(o.get("version"), 0, 64)
                    .unwrap_or("unknown")
                    .to_string(),
                scope: str_in(o.get("scope"), 1, 32).unwrap_or("user").to_string(),
                enabled: o.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                project_path,
                url: None,
                notes,
            })
        })
        .collect())
}

/// The arguments that enable or disable `id`, or None when its id cannot be passed safely.
pub fn mod_toggle_args(id: &str, scope: &str, enabled: bool) -> Option<Vec<String>> {
    mod_args(if enabled { "enable" } else { "disable" }, id, scope)
}

/// `plugin <verb> <id>`, with `--scope` where claude takes one.
fn mod_args(verb: &str, id: &str, scope: &str) -> Option<Vec<String>> {
    if !MOD_ID.is_match(id) {
        return None;
    }
    let mut args = strings(&["plugin", verb, id]);
    if SCOPE_FLAGS.contains(&scope) {
        args.extend(strings(&["--scope", scope]));
    }
    Some(args)
}

pub fn list_mods(run: &dyn ClaudeRunner, env: &Env, cwd: &Path) -> Result<Vec<ModInfo>, String> {
    let args = strings(&["plugin", "list", "--json"]);
    let opts = RunOptions {
        env,
        cwd,
        timeout: TIMEOUT,
    };
    parse_mod_list(&run.run(&args, &opts)?)
}

/// Installed mods with their marketplace's page as link: the installed list does not say more.
pub fn with_mod_links(mods: Vec<ModInfo>, marketplaces: &[Marketplace]) -> Vec<ModInfo> {
    mods.into_iter()
        .map(|m| {
            let market = marketplace_of(&m.id);
            let url = marketplaces
                .iter()
                .find(|x| x.name == market)
                .and_then(|x| x.url.clone());
            ModInfo { url, ..m }
        })
        .collect()
}

// ---- The catalog ------------------------------------------------------------------------

/// A folder inside a repository page; GitHub is the only host whose folder address is known.
fn in_repo(page: &str, dir: &str, rev: &str) -> String {
    // dir.replace(/^\.?\/+/, '').replace(/\/+$/, '')
    let lead = dir.strip_prefix('.').unwrap_or(dir);
    let clean = if lead.starts_with('/') {
        lead.trim_start_matches('/')
    } else {
        dir
    };
    let clean = clean.trim_end_matches('/');
    if clean.is_empty()
        || !page.starts_with("https://github.com/")
        || clean.split('/').any(|p| p == "..")
    {
        return page.to_string();
    }
    let at = if REV.is_match(rev) { rev } else { "HEAD" };
    let path: Vec<String> = clean.split('/').map(encode_uri_component).collect();
    format!("{page}/tree/{at}/{}", path.join("/"))
}

/// The page of a catalog entry's source: its own repository (and folder in it), or for a mod
/// that lives inside its marketplace, that folder of the marketplace's repository.
fn source_url(source: Option<&Value>, marketplace: Option<&str>) -> Option<String> {
    match source? {
        Value::String(s) => marketplace.map(|m| in_repo(m, s, "HEAD")),
        Value::Object(o) => {
            let page = https_page(o.get("url").and_then(Value::as_str))?;
            let Some(path) = o.get("path").and_then(Value::as_str) else {
                return Some(page);
            };
            let rev = o.get("ref").and_then(Value::as_str).unwrap_or("HEAD");
            Some(in_repo(&page, path, rev))
        }
        _ => None,
    }
}

/// Reads the `available` half of `claude plugin list --available --json`.
pub fn parse_mod_catalog(
    stdout: &str,
    marketplaces: &[Marketplace],
) -> Result<Vec<CatalogMod>, String> {
    const NONE: &str = "claude printed no mod catalog";
    let raw = json_from(stdout, '{', NONE)?;
    let list = raw.get("available").and_then(Value::as_array).ok_or(NONE)?;
    Ok(list
        .iter()
        .filter_map(|item| {
            let o = item.as_object()?;
            let id = str_in(o.get("pluginId"), 1, 200)?;
            let name = str_in(o.get("name"), 1, 120)?;
            if !MOD_ID.is_match(id) {
                return None;
            }
            let marketplace = str_in(o.get("marketplaceName"), 0, 120).unwrap_or("");
            let market_url = marketplaces
                .iter()
                .find(|m| m.name == marketplace)
                .and_then(|m| m.url.as_deref());
            Some(CatalogMod {
                id: id.to_string(),
                name: name.to_string(),
                description: description(o.get("description")),
                marketplace: marketplace.to_string(),
                version: nullish_str(o.get("version"), 64),
                installs: count_of(o.get("installCount")),
                stars: None,
                clone: None,
                url: source_url(o.get("source"), market_url),
            })
        })
        .collect())
}

pub fn list_mod_catalog(
    run: &dyn ClaudeRunner,
    env: &Env,
    cwd: &Path,
    marketplaces: &[Marketplace],
) -> Result<Vec<CatalogMod>, String> {
    let args = strings(&["plugin", "list", "--available", "--json"]);
    let opts = RunOptions {
        env,
        cwd,
        timeout: CATALOG_TIMEOUT,
    };
    parse_mod_catalog(&run.run(&args, &opts)?, marketplaces)
}

/// Installs for the whole account (user scope), or None when the id cannot be passed safely.
pub fn mod_install_args(id: &str) -> Option<Vec<String>> {
    MOD_ID
        .is_match(id)
        .then(|| strings(&["plugin", "install", id, "--scope", "user"]))
}

/// The arguments that uninstall (`update` false) or update a listed mod in its own scope, or
/// None when unsafe.
pub fn mod_change_args(id: &str, scope: &str, update: bool) -> Option<Vec<String>> {
    mod_args(if update { "update" } else { "uninstall" }, id, scope)
}

/// Every account's catalog as one list: the first account to offer an id keeps its row.
pub fn merge_catalogs(lists: Vec<Vec<CatalogMod>>) -> Vec<CatalogMod> {
    let mut seen = std::collections::HashSet::new();
    lists
        .into_iter()
        .flatten()
        .filter(|m| seen.insert(m.id.clone()))
        .collect()
}

/// The line of claude's error output worth showing: its last non-empty one.
pub fn last_line(text: &str) -> Option<String> {
    text.split('\n')
        .map(crate::util::js_trim)
        .rfind(|l| !l.is_empty())
        .map(|l| js_slice(l, 300))
}

// ---- The real claude ------------------------------------------------------------------

/// What `spawnCommandFor` (platform.ts) makes of a command: the program, and its arguments
/// either as a list or, for `cmd.exe`, as one verbatim line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpawnArgs {
    List(Vec<String>),
    Verbatim(String),
}

/// Characters cmd.exe would read as its own.
fn cmd_unsafe(a: &str) -> bool {
    a.contains(['&', '|', '<', '>', '^', '%', '"', '\r', '\n'])
}

/// How to run `file` on `windows` or not: as itself, except a `.cmd`/`.bat` shim, which runs
/// through cmd.exe and so must not carry characters cmd.exe would read.
pub fn spawn_command_for(
    windows: bool,
    file: &str,
    args: &[String],
    comspec: Option<&str>,
) -> Result<(String, SpawnArgs), String> {
    let lower = file.to_ascii_lowercase();
    if !windows || !(lower.ends_with(".cmd") || lower.ends_with(".bat")) {
        return Ok((file.to_string(), SpawnArgs::List(args.to_vec())));
    }
    if let Some(bad) = std::iter::once(file)
        .chain(args.iter().map(String::as_str))
        .find(|a| cmd_unsafe(a))
    {
        let base = file.rsplit(['/', '\\']).next().unwrap_or(file);
        return Err(format!(
            "\"{bad}\" contains characters that cannot pass through {base}. Remove them or install the native claude.exe."
        ));
    }
    let quote = |a: &str| {
        if a.is_empty() || a.chars().any(crate::util::is_js_space) {
            format!("\"{a}\"")
        } else {
            a.to_string()
        }
    };
    let line: Vec<String> = std::iter::once(format!("\"{file}\""))
        .chain(args.iter().map(|a| quote(a)))
        .collect();
    let shell = comspec.filter(|c| !c.is_empty()).unwrap_or("cmd.exe");
    Ok((
        shell.to_string(),
        SpawnArgs::Verbatim(format!("/d /s /c \"{}\"", line.join(" "))),
    ))
}

/// The catalog of every marketplace mod runs to a few megabytes.
const MAX_BUFFER: usize = 32 * 1024 * 1024;

/// The claude binary at `file`, run without a shell (cmd.exe only for claude.cmd).
#[derive(Clone, Debug)]
pub struct ClaudeCli {
    pub file: PathBuf,
}

impl ClaudeCli {
    pub fn new(file: impl Into<PathBuf>) -> Self {
        ClaudeCli { file: file.into() }
    }
}

/// Reads a pipe up to MAX_BUFFER; past it, flags `over` so the child is stopped.
fn drain(
    mut pipe: impl Read + Send + 'static,
    over: Arc<AtomicBool>,
) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut out = Vec::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = MAX_BUFFER.saturating_sub(out.len());
                    out.extend_from_slice(&buf[..n.min(room)]);
                    if n > room {
                        over.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
        }
        out
    })
}

/// A spawn error as Node words it: `spawn claude ENOENT`.
fn spawn_error(file: &str, e: &std::io::Error) -> String {
    let code = match e.kind() {
        std::io::ErrorKind::NotFound => "ENOENT".to_string(),
        std::io::ErrorKind::PermissionDenied => "EACCES".to_string(),
        _ => e.to_string(),
    };
    format!("spawn {file} {code}")
}

impl ClaudeRunner for ClaudeCli {
    fn run(&self, args: &[String], opts: &RunOptions<'_>) -> Result<String, String> {
        let comspec = std::env::var("ComSpec").ok();
        let file = self.file.to_string_lossy();
        let (program, spawn_args) =
            spawn_command_for(cfg!(windows), &file, args, comspec.as_deref())?;
        let mut cmd = Command::new(&program);
        match &spawn_args {
            SpawnArgs::List(list) => {
                cmd.args(list);
            }
            #[cfg(windows)]
            SpawnArgs::Verbatim(line) => {
                std::os::windows::process::CommandExt::raw_arg(&mut cmd, line);
            }
            #[cfg(not(windows))]
            SpawnArgs::Verbatim(line) => {
                cmd.arg(line);
            }
        }
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW, as windowsHide: no console flashes up.
            std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
        }
        cmd.current_dir(opts.cwd)
            .env_clear()
            .envs(opts.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| spawn_error(&program, &e))?;

        let over = Arc::new(AtomicBool::new(false));
        let out = drain(child.stdout.take().expect("piped"), over.clone());
        let err = drain(child.stderr.take().expect("piped"), over.clone());
        let deadline = Instant::now() + opts.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline && !over.load(Ordering::Relaxed) => {
                    thread::sleep(Duration::from_millis(10));
                }
                // Timed out, outran the buffer, or cannot be waited on: stop it.
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let stdout = String::from_utf8_lossy(&out.join().unwrap_or_default()).into_owned();
        let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default()).into_owned();
        if status.is_some_and(|s| s.success()) {
            return Ok(stdout);
        }
        let message = if over.load(Ordering::Relaxed) {
            "stdout maxBuffer length exceeded".to_string()
        } else {
            let line = match &spawn_args {
                SpawnArgs::List(list) => list.join(" "),
                SpawnArgs::Verbatim(line) => line.clone(),
            };
            let sep = if line.is_empty() { "" } else { " " };
            format!("Command failed: {program}{sep}{line}\n{stderr}")
        };
        Err(last_line(&stderr)
            .or_else(|| last_line(&stdout))
            .unwrap_or(message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    #[test]
    fn mod_list_keeps_what_the_view_shows_and_skips_what_is_not_a_mod() {
        let listed = json!([
            { "id": "tally@acme", "version": "0.2.0", "scope": "user", "enabled": true, "installPath": "/x" },
            { "id": "tally@acme", "version": "0.2.0", "scope": "project", "enabled": false, "projectPath": "/p" },
            { "id": "eng@synced", "scope": "synced", "enabled": true, "notes": ["shadowed", "x".repeat(900)] },
            { "id": "--all", "version": "1", "scope": "user", "enabled": true },
            { "version": "1" },
            "junk"
        ])
        .to_string();
        let mods = parse_mod_list(&listed).unwrap();
        let rows: Vec<_> = mods
            .iter()
            .map(|m| {
                (
                    m.id.as_str(),
                    m.scope.as_str(),
                    m.enabled,
                    m.project_path.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("tally@acme", "user", true, None),
                ("tally@acme", "project", false, Some("/p")),
                ("eng@synced", "synced", true, None)
            ]
        );
        assert_eq!(mods[2].version, "unknown");
        assert_eq!(mods[2].notes[1].len(), 300);

        // Text printed before the list is skipped; output without one is refused.
        assert_eq!(
            parse_mod_list(&format!("warning: slow disk\n{listed}"))
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            parse_mod_list("not logged in").unwrap_err(),
            "claude printed no mod list"
        );
        assert!(parse_mod_list("{\"a\":1}").is_err());
    }

    #[test]
    fn a_bad_project_path_drops_the_entry_and_bad_fields_fall_back() {
        let mods = parse_mod_list(
            r#"[{"id":"a@x","projectPath":null},{"id":"b@x","version":7,"scope":"","enabled":"no","notes":[1]}]"#,
        )
        .unwrap();
        assert_eq!(mods.len(), 1);
        assert_eq!(
            (
                mods[0].version.as_str(),
                mods[0].scope.as_str(),
                mods[0].enabled
            ),
            ("unknown", "user", true)
        );
        assert!(mods[0].notes.is_empty());
    }

    #[test]
    fn list_mods_asks_the_given_claude() {
        let calls = Mutex::new(Vec::new());
        let run = |args: &[String], _: &RunOptions<'_>| {
            calls.lock().unwrap().push(args.to_vec());
            Ok("[]".to_string())
        };
        let mods = list_mods(&run, &Env::new(), Path::new("/home/tester")).unwrap();
        assert!(mods.is_empty());
        assert_eq!(
            calls.into_inner().unwrap(),
            [strings(&["plugin", "list", "--json"])]
        );
    }

    #[test]
    fn toggle_names_the_scope_only_where_claude_takes_one() {
        assert_eq!(
            mod_toggle_args("tally@acme", "user", false).unwrap(),
            strings(&["plugin", "disable", "tally@acme", "--scope", "user"])
        );
        assert_eq!(
            mod_toggle_args("eng@synced", "synced", true).unwrap(),
            strings(&["plugin", "enable", "eng@synced"])
        );
        assert_eq!(mod_toggle_args("--all", "user", false), None);
        assert_eq!(mod_toggle_args("a&calc@x", "user", false), None);
    }

    #[test]
    fn last_line_is_the_last_non_empty_line() {
        assert_eq!(
            last_line("a\nPlugin not found\n\n").as_deref(),
            Some("Plugin not found")
        );
        assert_eq!(last_line(" \n"), None);
    }

    #[test]
    fn catalog_keeps_what_the_list_shows_and_skips_what_is_not_installable() {
        let printed = json!({
            "installed": [],
            "available": [
                {
                    "pluginId": "clock@acme", "name": "clock",
                    "description": " A clock\n above the prompt ", "marketplaceName": "acme",
                    "version": "1.1.0", "installCount": 4520,
                    "source": { "source": "url", "url": "https://example.com/clock.git" }
                },
                { "pluginId": "weather@acme", "name": "weather", "marketplaceName": "acme", "source": "./weather" },
                { "pluginId": "ssh@acme", "name": "ssh", "source": { "url": "ssh://git@example.com/x" } },
                { "pluginId": "-y", "name": "bad", "source": "." },
                { "name": "no id" }
            ]
        })
        .to_string();
        let row = |id: &str, name: &str, market: &str| CatalogMod {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            marketplace: market.into(),
            version: None,
            installs: None,
            stars: None,
            url: None,
            clone: None,
        };
        assert_eq!(
            parse_mod_catalog(&format!("note\n{printed}"), &[]).unwrap(),
            [
                CatalogMod {
                    description: "A clock above the prompt".into(),
                    version: Some("1.1.0".into()),
                    installs: Some(4520),
                    url: Some("https://example.com/clock".into()),
                    ..row("clock@acme", "clock", "acme")
                },
                row("weather@acme", "weather", "acme"),
                row("ssh@acme", "ssh", "")
            ]
        );
        assert!(parse_mod_catalog("[]", &[]).is_err());
        assert_eq!(
            parse_mod_catalog("{\"installed\":[]}", &[]).unwrap_err(),
            "claude printed no mod catalog"
        );
    }

    #[test]
    fn installs_for_the_account_never_an_option() {
        assert_eq!(
            mod_install_args("clock@acme").unwrap(),
            strings(&["plugin", "install", "clock@acme", "--scope", "user"])
        );
        assert_eq!(mod_install_args("--yes"), None);
    }

    #[test]
    fn links_a_mod_to_its_repository_and_its_folder_there() {
        let markets = parse_marketplaces(&format!(
            "note\n{}",
            json!([
                { "name": "acme", "source": "github", "repo": "acme/mods", "installLocation": "/x" },
                { "name": "git", "source": "git", "url": "https://git.example.com/mods.git" },
                { "name": "local", "source": "directory", "path": "/home/me/mods" },
                { "name": "odd", "source": "github", "repo": "--upload-pack=x" }
            ])
        ))
        .unwrap();
        let m = |name: &str, url: Option<&str>, add: Option<&str>| Marketplace {
            name: name.into(),
            url: url.map(Into::into),
            add: add.map(Into::into),
        };
        assert_eq!(
            markets,
            [
                m(
                    "acme",
                    Some("https://github.com/acme/mods"),
                    Some("acme/mods")
                ),
                m(
                    "git",
                    Some("https://git.example.com/mods"),
                    Some("https://git.example.com/mods.git")
                ),
                m("local", None, None),
                m("odd", None, None)
            ]
        );
        let entry = |name: &str, market: &str, source: Value| json!({ "pluginId": format!("{name}@{market}"), "name": name, "marketplaceName": market, "source": source });
        let sub = json!({ "source": "git-subdir", "url": "https://github.com/o/r.git", "path": "plugins/a b" });
        let with = |extra: Value| {
            let mut s = sub.clone();
            s.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            s
        };
        let printed = json!({ "available": [
            entry("inside", "acme", json!("./plugins/inside")),
            entry("root", "acme", json!("./")),
            entry("up", "acme", json!("../../etc")),
            entry("elsewhere", "git", json!("./x")),
            entry("homeless", "local", json!("./x")),
            entry("sub", "acme", with(json!({ "ref": "v1.5.5", "sha": "abc" }))),
            entry("sub-head", "acme", sub.clone()),
            entry("sub-odd", "acme", with(json!({ "ref": "a b?" })))
        ]})
        .to_string();
        let urls: Vec<_> = parse_mod_catalog(&printed, &markets)
            .unwrap()
            .into_iter()
            .map(|m| m.url)
            .collect();
        let s = |u: &str| Some(u.to_string());
        assert_eq!(
            urls,
            [
                s("https://github.com/acme/mods/tree/HEAD/plugins/inside"),
                s("https://github.com/acme/mods"),
                s("https://github.com/acme/mods"),
                s("https://git.example.com/mods"),
                None,
                s("https://github.com/o/r/tree/v1.5.5/plugins/a%20b"),
                s("https://github.com/o/r/tree/HEAD/plugins/a%20b"),
                s("https://github.com/o/r/tree/HEAD/plugins/a%20b")
            ]
        );
        let linked = with_mod_links(
            parse_mod_list(
                r#"[{"id":"tally@acme","scope":"user"},{"id":"eng@synced","scope":"synced"}]"#,
            )
            .unwrap(),
            &markets,
        );
        assert_eq!(
            linked[0].url.as_deref(),
            Some("https://github.com/acme/mods")
        );
        assert_eq!(linked[1].url, None);
    }

    #[test]
    fn adds_a_marketplace_by_repo_or_https_address_only() {
        assert_eq!(
            marketplace_add_args("acme/mods").unwrap(),
            strings(&["plugin", "marketplace", "add", "acme/mods"])
        );
        assert_eq!(
            marketplace_add_args("https://git.example.com/mods.git")
                .unwrap()
                .len(),
            4
        );
        assert_eq!(marketplace_add_args("--scope"), None);
        assert_eq!(marketplace_add_args("/home/me/mods"), None);
        assert_eq!(marketplace_add_args("ssh://git@example.com/x"), None);
        assert_eq!(marketplace_add_args("https://me:pw@example.com/x"), None);
    }

    #[test]
    fn changes_in_the_mods_own_scope_never_an_unsafe_id_or_name() {
        assert_eq!(
            mod_change_args("clock@acme", "user", false).unwrap(),
            strings(&["plugin", "uninstall", "clock@acme", "--scope", "user"])
        );
        assert_eq!(
            mod_change_args("clock@acme", "managed", true).unwrap(),
            strings(&["plugin", "update", "clock@acme"])
        );
        assert_eq!(mod_change_args("--all", "user", false), None);
        assert_eq!(
            marketplace_remove_args("acme").unwrap(),
            strings(&["plugin", "marketplace", "remove", "acme"])
        );
        assert_eq!(marketplace_remove_args("-rf"), None);
        assert_eq!(marketplace_remove_args("a b"), None);
    }

    #[test]
    fn merges_catalogs_first_listing_of_an_id_wins() {
        let row = |id: &str, installs: u64| CatalogMod {
            id: id.into(),
            name: mod_name_of(id).into(),
            description: String::new(),
            marketplace: marketplace_of(id).into(),
            version: None,
            installs: Some(installs),
            stars: None,
            url: None,
            clone: None,
        };
        let merged = merge_catalogs(vec![
            vec![row("a@x", 1), row("b@x", 2)],
            vec![row("b@x", 9), row("c@y", 3)],
        ]);
        let got: Vec<_> = merged.iter().map(|m| (m.id.as_str(), m.installs)).collect();
        assert_eq!(got, [("a@x", Some(1)), ("b@x", Some(2)), ("c@y", Some(3))]);
    }

    #[test]
    fn mod_id_halves_and_downloaded_folders() {
        assert_eq!(
            (mod_name_of("a@b@c"), marketplace_of("a@b@c")),
            ("a@b", "c")
        );
        assert_eq!((mod_name_of("@x"), marketplace_of("@x")), ("@x", ""));
        assert_eq!(
            downloaded_mod_name("~/.wraithgrid/mods/burn-meter"),
            Some("burn-meter")
        );
        assert_eq!(
            downloaded_mod_name("~\\.wraithgrid\\mods\\burn-meter\\"),
            Some("burn-meter")
        );
        assert_eq!(downloaded_mod_name("~/mods/tally"), None);
        assert_eq!(downloaded_mod_name("~/.wraithgrid/mods/a/b"), None);
        assert_eq!(downloaded_mod_name("~/.wraithgrid/mods/.."), None);
        assert_eq!(downloaded_mod_name("/home/x/.wraithgrid/mods/m"), None);
    }

    #[test]
    fn install_counts_are_safe_integers() {
        assert_eq!(count_of(Some(&json!(4520.0))), Some(4520));
        assert_eq!(count_of(Some(&json!(-1))), None);
        assert_eq!(count_of(Some(&json!(1.5))), None);
        assert_eq!(count_of(Some(&json!(1e300))), None);
        assert_eq!(count_of(Some(&json!("4"))), None);
    }

    #[test]
    fn cmd_shims_go_through_cmd_exe_quoted() {
        let args = strings(&["plugin", "install", "a b@x"]);
        assert_eq!(
            spawn_command_for(false, "claude.cmd", &args, None).unwrap(),
            ("claude.cmd".into(), SpawnArgs::List(args.clone()))
        );
        assert_eq!(
            spawn_command_for(true, r"C:\n\claude.CMD", &args, Some(r"C:\W\cmd.exe")).unwrap(),
            (
                r"C:\W\cmd.exe".into(),
                SpawnArgs::Verbatim(
                    r#"/d /s /c ""C:\n\claude.CMD" plugin install "a b@x"""#.into()
                )
            )
        );
        assert_eq!(
            spawn_command_for(true, r"C:\n\claude.cmd", &strings(&["a&b"]), None).unwrap_err(),
            "\"a&b\" contains characters that cannot pass through claude.cmd. Remove them or install the native claude.exe."
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_real_runner_returns_stdout_or_the_last_error_line() {
        let dir = tempfile::tempdir().unwrap();
        // Run through /bin/sh: exec'ing a file just written can fail with ETXTBSY.
        let script = dir.path().join("claude.sh");
        std::fs::write(
            &script,
            "if [ \"$1\" = fail ]; then echo 'Error:' >&2; echo \"no $X\" >&2; exit 1; fi\nif [ \"$1\" = slow ]; then sleep 5; fi\necho \"$@ $X $(pwd)\"\n",
        )
        .unwrap();
        let sh = |args: &[&str]| {
            let mut all = vec![script.to_string_lossy().into_owned()];
            all.extend(strings(args));
            all
        };
        let claude = ClaudeCli::new("/bin/sh");
        let env = Env::from([("X".to_string(), "y".to_string())]);
        let opts = RunOptions {
            env: &env,
            cwd: dir.path(),
            timeout: TIMEOUT,
        };
        let cwd = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            claude.run(&sh(&["a", "b"]), &opts).unwrap(),
            format!("a b y {}\n", cwd.display())
        );
        assert_eq!(claude.run(&sh(&["fail"]), &opts).unwrap_err(), "no y");
        let quick = RunOptions {
            timeout: Duration::from_millis(200),
            ..opts
        };
        let err = claude.run(&sh(&["slow"]), &quick).unwrap_err();
        assert!(err.starts_with("Command failed: /bin/sh "), "{err}");
        let missing = ClaudeCli::new(dir.path().join("nope"));
        assert!(missing.run(&[], &opts).unwrap_err().ends_with(" ENOENT"));
    }
}
