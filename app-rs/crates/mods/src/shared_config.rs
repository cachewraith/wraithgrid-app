//! "Overall" sharing: one brain per provider (port of `src/main/shared-config.ts` and the
//! shared-mode handlers of `ipc.ts`). Every claude account shares one CLAUDE.md, skills,
//! plugins…; every codex account one AGENTS.md and skills; and so on. The real files live in
//! ~/.wraithgrid/shared/<cli>; each account's config dir gets links to them. Logins and
//! history stay per account. Nothing inside a config dir is ever read: only link metadata is
//! inspected, and a real file in the way is renamed to a backup, never deleted. The links and
//! backups are byte-for-byte what the Electron app makes, so switching apps changes nothing.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::util::{iso_timestamp, join, resolve, resolve_user_path};
use crate::Account;

/// Whether an item is a file or a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    File,
    Dir,
}

/// One file or folder a CLI's accounts share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedItem {
    pub name: &'static str,
    pub kind: ItemKind,
    /// What a missing file starts as.
    pub initial: Option<&'static str>,
}

const fn file(name: &'static str, initial: &'static str) -> SharedItem {
    SharedItem {
        name,
        kind: ItemKind::File,
        initial: Some(initial),
    }
}

const fn dir(name: &'static str) -> SharedItem {
    SharedItem {
        name,
        kind: ItemKind::Dir,
        initial: None,
    }
}

/// What claude accounts share.
pub const SHARED_ITEMS: &[SharedItem] = &[
    file("CLAUDE.md", ""),
    file("settings.json", "{}\n"),
    dir("skills"),
    dir("plugins"),
    dir("agents"),
    dir("commands"),
];

/// How one CLI shares across its accounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShareSpec {
    /// The CLI's own home (under the user's home dir): seeds the shared copy once.
    pub home: &'static str,
    /// Where in an account's config dir the CLI reads these ('' = the dir itself).
    pub sub: &'static str,
    pub items: &'static [SharedItem],
}

const CLAUDE: ShareSpec = ShareSpec {
    home: ".claude",
    sub: "",
    items: SHARED_ITEMS,
};
const CODEX: ShareSpec = ShareSpec {
    home: ".codex",
    sub: "",
    items: &[file("AGENTS.md", ""), dir("skills"), dir("prompts")],
};
const GEMINI: ShareSpec = ShareSpec {
    home: ".gemini",
    sub: ".gemini",
    items: &[file("GEMINI.md", ""), dir("commands"), dir("skills")],
};
const COPILOT: ShareSpec = ShareSpec {
    home: ".copilot",
    sub: "",
    items: &[
        file("copilot-instructions.md", ""),
        dir("agents"),
        dir("skills"),
    ],
};

/// What each CLI shares across its accounts. claude and codex are checked against the real
/// CLIs (codex 0.164 reads $CODEX_HOME/AGENTS.md, skills and prompts); gemini and copilot
/// follow their docs (GEMINI_CLI_HOME/.gemini, COPILOT_HOME). CLIs without a per-account
/// config dir (agy, opencode) already share one home; cursor has no documented global file.
pub fn share_spec(cli: &str) -> Option<&'static ShareSpec> {
    match cli {
        "claude" => Some(&CLAUDE),
        "codex" => Some(&CODEX),
        "gemini" => Some(&GEMINI),
        "copilot" => Some(&COPILOT),
        _ => None,
    }
}

/// Where a CLI's shared brain lives: ~/.wraithgrid/shared/<cli>.
pub fn shared_dir(home_dir: &Path, cli: &str) -> PathBuf {
    join(&home_dir.join(".wraithgrid").join("shared"), cli)
}

/// The CLI's own home; claude accounts used to link straight to ~/.claude.
pub fn legacy_source_dir(home_dir: &Path, spec: &ShareSpec) -> PathBuf {
    join(home_dir, spec.home)
}

/// `mkdir -p` with mode 0700 for every folder it creates.
fn mkdir_private(p: &Path) -> io::Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(p)
}

/// Makes the shared dir for a CLI: an item it doesn't have yet is copied once from the CLI's
/// own home (links kept as links), else created empty. The CLI's home is never changed.
pub fn prepare_shared(home_dir: &Path, cli: &str, spec: &ShareSpec) -> io::Result<PathBuf> {
    let dir = shared_dir(home_dir, cli);
    mkdir_private(&dir)?;
    let from = legacy_source_dir(home_dir, spec);
    for item in spec.items {
        let dst = join(&dir, item.name);
        if fs::symlink_metadata(&dst).is_ok() {
            continue;
        }
        let src = join(&from, item.name);
        let Ok(st) = fs::metadata(&src) else { continue };
        if st.is_dir() != (item.kind == ItemKind::Dir) {
            continue;
        }
        copy_verbatim(&src, &dst)?;
    }
    ensure_source(&dir, spec.items)?;
    Ok(dir)
}

/// `fs.cp(src, dst, { recursive: true, verbatimSymlinks: true })` into a fresh `dst`: files
/// and folders copied with their modes, links recreated with their target text unchanged.
fn copy_verbatim(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        let target = fs::read_link(src)?;
        return symlink_verbatim(&target, src, dst);
    }
    if ft.is_dir() {
        if fs::symlink_metadata(dst).is_err() {
            fs::create_dir(dst)?;
        }
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_verbatim(&entry.path(), &dst.join(entry.file_name()))?;
        }
        return fs::set_permissions(dst, meta.permissions());
    }
    if ft.is_file() {
        return fs::copy(src, dst).map(|_| ());
    }
    Err(io::Error::other(format!(
        "cannot copy {}: not a file, folder or link",
        src.display()
    )))
}

#[cfg(unix)]
fn symlink_verbatim(target: &Path, _src: &Path, dst: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, dst)
}

#[cfg(windows)]
fn symlink_verbatim(target: &Path, _src: &Path, dst: &Path) -> io::Result<()> {
    // Node picks a dir or file link by what the target is, seen from the new link.
    let at = dst.parent().unwrap_or(Path::new("")).join(target);
    if fs::metadata(at).map(|m| m.is_dir()).unwrap_or(false) {
        std::os::windows::fs::symlink_dir(target, dst)
    } else {
        std::os::windows::fs::symlink_file(target, dst)
    }
}

/// What happened to one item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkOutcome {
    Linked,
    AlreadyLinked,
    Unlinked,
    Restored,
    Failed,
}

/// The outcome for one item of one account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemResult {
    pub item: String,
    pub outcome: LinkOutcome,
    /// Where an existing file or folder was moved to make room for the link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ItemResult {
    fn new(item: &str, outcome: LinkOutcome) -> Self {
        ItemResult {
            item: item.to_string(),
            outcome,
            backup: None,
            error: None,
        }
    }

    fn failed(item: &str, err: io::Error) -> Self {
        ItemResult {
            error: Some(err.to_string()),
            ..Self::new(item, LinkOutcome::Failed)
        }
    }
}

pub const BACKUP_SUFFIX: &str = ".wraithgrid-backup";

fn exists_no_follow(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

/// `p` + `suffix`, as string concatenation.
fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = OsString::from(p.as_os_str());
    s.push(suffix);
    PathBuf::from(s)
}

/// A link target as Node's readlink reports it: Windows junctions read back as `\\?\C:\…`.
fn plain_target(target: PathBuf) -> PathBuf {
    if cfg!(windows) {
        let s = target.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    target
}

/// True when `dst` already points at `src` (symlink or junction, or a Windows hard link).
fn is_link_to(dst: &Path, src: &Path) -> bool {
    let Ok(st) = fs::symlink_metadata(dst) else {
        return false;
    };
    if st.file_type().is_symlink() {
        let Ok(target) = fs::read_link(dst) else {
            return false;
        };
        let base = dst.parent().unwrap_or(Path::new(""));
        return resolve(&base.join(plain_target(target))) == resolve(src);
    }
    // A hard link (Windows fallback) shares the source's inode.
    if st.is_file() {
        let Ok(s) = fs::symlink_metadata(src) else {
            return false;
        };
        return s.is_file() && same_inode(&s, &st, src, dst);
    }
    false
}

#[cfg(unix)]
fn same_inode(s: &fs::Metadata, st: &fs::Metadata, _src: &Path, _dst: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    s.ino() == st.ino() && s.dev() == st.dev() && st.nlink() > 1
}

#[cfg(windows)]
fn same_inode(_s: &fs::Metadata, _st: &fs::Metadata, src: &Path, dst: &Path) -> bool {
    // Two names of one file: the same file identity means a link count above one.
    same_file::is_same_file(src, dst).unwrap_or(false)
}

/// A free backup name next to `p`: `name.wraithgrid-backup`, then with a timestamp.
fn backup_path_for(p: &Path, now: SystemTime) -> PathBuf {
    let plain = with_suffix(p, BACKUP_SUFFIX);
    if !exists_no_follow(&plain) {
        return plain;
    }
    let stamp = iso_timestamp(now).replace([':', '.'], "-");
    with_suffix(&plain, &format!("-{stamp}"))
}

/// Makes sure the source has something to link to for every item. Never overwrites.
pub fn ensure_source(source_dir: &Path, items: &[SharedItem]) -> io::Result<()> {
    mkdir_private(source_dir)?;
    for item in items {
        let p = join(source_dir, item.name);
        if item.kind == ItemKind::Dir {
            mkdir_private(&p)?;
            continue;
        }
        // create_new makes the file only if it does not exist; an existing file is untouched.
        let mut o = fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        match o.open(&p) {
            Ok(mut f) => io::Write::write_all(&mut f, item.initial.unwrap_or("").as_bytes())?,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn create_link(src: &Path, dst: &Path, _kind: ItemKind) -> io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn create_link(src: &Path, dst: &Path, kind: ItemKind) -> io::Result<()> {
    // Junctions need no privileges; file symlinks need Developer Mode, else a hard link.
    if kind == ItemKind::Dir {
        return junction::create(src, dst);
    }
    match std::os::windows::fs::symlink_file(src, dst) {
        Ok(()) => Ok(()),
        // ERROR_PRIVILEGE_NOT_HELD, which Node reports as EPERM.
        Err(e) if e.raw_os_error() == Some(1314) || e.kind() == io::ErrorKind::PermissionDenied => {
            fs::hard_link(src, dst)
        }
        Err(e) => Err(e),
    }
}

/// Removes a link itself, never what it points at (a junction is removed as a folder entry).
fn remove_link(p: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if fs::symlink_metadata(p)?.file_type().is_symlink_dir() {
            return fs::remove_dir(p);
        }
    }
    fs::remove_file(p)
}

/// Links the shared items from `source_dir` into `target_dir`. Idempotent.
pub fn link_shared(
    source_dir: &Path,
    target_dir: &Path,
    items: &[SharedItem],
) -> io::Result<Vec<ItemResult>> {
    link_shared_at(source_dir, target_dir, items, &SystemTime::now)
}

/// [`link_shared`] with the clock that names a second backup.
pub fn link_shared_at(
    source_dir: &Path,
    target_dir: &Path,
    items: &[SharedItem],
    now: &dyn Fn() -> SystemTime,
) -> io::Result<Vec<ItemResult>> {
    if resolve(source_dir) == resolve(target_dir) {
        return Ok(Vec::new());
    }
    mkdir_private(target_dir)?;

    let mut results = Vec::new();
    for item in items {
        let src = join(source_dir, item.name);
        let dst = join(target_dir, item.name);
        if is_link_to(&dst, &src) {
            results.push(ItemResult::new(item.name, LinkOutcome::AlreadyLinked));
            continue;
        }
        let step = || -> io::Result<Option<PathBuf>> {
            let mut backup = None;
            match fs::symlink_metadata(&dst) {
                // A link to somewhere else (e.g. an older source) holds no data of its own.
                Ok(m) if m.file_type().is_symlink() => remove_link(&dst)?,
                Ok(_) => {
                    let to = backup_path_for(&dst, now());
                    fs::rename(&dst, &to)?;
                    backup = Some(to);
                }
                Err(_) => {}
            }
            create_link(&src, &dst, item.kind)?;
            Ok(backup)
        };
        results.push(match step() {
            Ok(backup) => ItemResult {
                backup,
                ..ItemResult::new(item.name, LinkOutcome::Linked)
            },
            Err(e) => ItemResult::failed(item.name, e),
        });
    }
    Ok(results)
}

/// Removes links that point at `source_dir` and puts back what link_shared moved aside.
pub fn unlink_shared(
    source_dir: &Path,
    target_dir: &Path,
    items: &[SharedItem],
) -> Vec<ItemResult> {
    if resolve(source_dir) == resolve(target_dir) {
        return Vec::new();
    }
    let mut results = Vec::new();
    for item in items {
        let src = join(source_dir, item.name);
        let dst = join(target_dir, item.name);
        if !is_link_to(&dst, &src) {
            continue;
        }
        let step = || -> io::Result<ItemResult> {
            remove_link(&dst)?;
            let backup = with_suffix(&dst, BACKUP_SUFFIX);
            if exists_no_follow(&backup) {
                fs::rename(&backup, &dst)?;
                return Ok(ItemResult {
                    backup: Some(backup),
                    ..ItemResult::new(item.name, LinkOutcome::Restored)
                });
            }
            Ok(ItemResult::new(item.name, LinkOutcome::Unlinked))
        };
        results.push(step().unwrap_or_else(|e| ItemResult::failed(item.name, e)));
    }
    results
}

// ---- Per-account sharing (the shared-mode handlers of ipc.ts) -----------------------

/// `settings.sharedMode`: one brain per provider, or each account keeps its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SharedMode {
    #[default]
    Overall,
    PerAccount,
}

/// One line of the report Settings shows after switching the mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedReportEntry {
    /// The account's name.
    pub account: String,
    #[serde(flatten)]
    pub result: ItemResult,
}

/// The UI text when switching the mode fails as a whole.
pub const SHARED_APPLY_FAILED: &str = "Could not update the shared CLAUDE.md and skills";

/// One brain per provider (~/.wraithgrid/shared/<cli>), seeded once per run.
pub struct SharedBrains {
    home_dir: PathBuf,
    /// The shared dir of each CLI seeded so far. The lock is held while seeding, so panes
    /// that start together never copy at the same time.
    prepared: Mutex<HashMap<String, PathBuf>>,
}

impl SharedBrains {
    pub fn new(home_dir: impl Into<PathBuf>) -> Self {
        SharedBrains {
            home_dir: home_dir.into(),
            prepared: Mutex::new(HashMap::new()),
        }
    }

    /// The CLI's shared dir, seeded on first use; None for a CLI that shares nothing.
    pub fn shared_for(&self, cli: &str) -> Option<io::Result<PathBuf>> {
        let spec = share_spec(cli)?;
        let mut prepared = self.prepared.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(dir) = prepared.get(cli) {
            return Some(Ok(dir.clone()));
        }
        // A failed seed is not kept: the next account tries again.
        Some(prepare_shared(&self.home_dir, cli, spec).inspect(|dir| {
            prepared.insert(cli.to_string(), dir.clone());
        }))
    }

    /// Where an account's CLI reads the shared items: its config dir, or a folder in it.
    pub fn target_of(&self, cli: &str, config_dir: &str) -> PathBuf {
        let dir = resolve_user_path(config_dir, &self.home_dir);
        join(&dir, share_spec(cli).map_or("", |s| s.sub))
    }

    /// Takes an account's shared links out: today's and the old ~/.claude ones, restoring
    /// what they replaced. Called before an account's config dir is deleted, with the dir's
    /// real path, so a recursive delete can never reach through a link into the shared files.
    pub fn unshare(&self, cli: &str, config_dir: &str) -> Vec<ItemResult> {
        let Some(spec) = share_spec(cli) else {
            return Vec::new();
        };
        let target = self.target_of(cli, config_dir);
        let mut out = unlink_shared(&shared_dir(&self.home_dir, cli), &target, spec.items);
        out.extend(unlink_shared(
            &legacy_source_dir(&self.home_dir, spec),
            &target,
            spec.items,
        ));
        out
    }

    /// Links its provider's shared instructions and skills into one account, if sharing is
    /// overall (before a pane of it starts). Problems are logged, never raised.
    pub fn sync_shared(&self, mode: SharedMode, account: &Account, log: &dyn Fn(&str)) {
        if mode != SharedMode::Overall {
            return;
        }
        let (Some(spec), Some(source)) = (share_spec(&account.cli), self.shared_for(&account.cli))
        else {
            return;
        };
        let target = self.target_of(&account.cli, &account.config_dir);
        let linked = source.and_then(|s| link_shared(&s, &target, spec.items));
        match linked {
            Ok(results) => {
                for r in results.iter().filter(|r| r.outcome == LinkOutcome::Failed) {
                    log(&format!(
                        "could not link shared {} for {}: {}",
                        r.item,
                        account.name,
                        r.error.as_deref().unwrap_or("")
                    ));
                }
            }
            Err(e) => log(&format!(
                "could not share instructions and skills with {}: {e}",
                account.name
            )),
        }
    }

    /// Switches every account to `mode`: overall links them all, per-account takes the links
    /// out and restores what they replaced. Err is the text Settings shows.
    pub fn apply(
        &self,
        mode: SharedMode,
        accounts: &[Account],
        log: &dyn Fn(&str),
    ) -> Result<Vec<SharedReportEntry>, String> {
        let run = || -> io::Result<Vec<SharedReportEntry>> {
            let mut report = Vec::new();
            for acc in accounts {
                let Some(spec) = share_spec(&acc.cli) else {
                    continue;
                };
                let results = match mode {
                    SharedMode::Overall => {
                        let source = self.shared_for(&acc.cli).expect("spec checked")?;
                        link_shared(
                            &source,
                            &self.target_of(&acc.cli, &acc.config_dir),
                            spec.items,
                        )?
                    }
                    // Take the links out, restoring what they replaced.
                    SharedMode::PerAccount => self.unshare(&acc.cli, &acc.config_dir),
                };
                report.extend(results.into_iter().map(|result| SharedReportEntry {
                    account: acc.name.clone(),
                    result,
                }));
            }
            Ok(report)
        };
        match run() {
            Ok(report) => {
                log(&match mode {
                    SharedMode::Overall => format!(
                        "sharing one brain per provider with {} accounts",
                        accounts.len()
                    ),
                    SharedMode::PerAccount => "stopped sharing instructions and skills".into(),
                });
                Ok(report)
            }
            Err(e) => {
                log(&format!("shared:apply failed: {e}"));
                Err(SHARED_APPLY_FAILED.into())
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::time::{Duration, UNIX_EPOCH};

    fn read(p: &Path) -> String {
        fs::read_to_string(p).unwrap()
    }

    fn outcomes(res: &[ItemResult]) -> Vec<(&str, LinkOutcome)> {
        res.iter().map(|r| (r.item.as_str(), r.outcome)).collect()
    }

    /// A source with rules, a skill and a credential, and a target beside it.
    struct Fixture {
        _root: tempfile::TempDir,
        root: PathBuf,
        source: PathBuf,
        target: PathBuf,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().to_path_buf();
        let source = base.join("personal");
        fs::create_dir_all(source.join("skills").join("deploy")).unwrap();
        fs::write(source.join("CLAUDE.md"), "# shared rules\n").unwrap();
        fs::write(source.join(".credentials.json"), "secret").unwrap();
        ensure_source(&source, SHARED_ITEMS).unwrap();
        Fixture {
            _root: root,
            target: base.join("work"),
            root: base,
            source,
        }
    }

    #[test]
    fn links_every_shared_item_into_an_account_and_is_idempotent() {
        let f = fixture();
        let first = link_shared(&f.source, &f.target, SHARED_ITEMS).unwrap();
        let want: Vec<_> = SHARED_ITEMS
            .iter()
            .map(|i| (i.name, LinkOutcome::Linked))
            .collect();
        assert_eq!(outcomes(&first), want);
        assert_eq!(
            fs::read_link(f.target.join("CLAUDE.md")).unwrap(),
            f.source.join("CLAUDE.md")
        );
        assert_eq!(read(&f.target.join("CLAUDE.md")), "# shared rules\n");
        assert!(f.target.join("skills").join("deploy").exists());
        // Credentials and everything else stay per account.
        assert!(!f.target.join(".credentials.json").exists());

        let again = link_shared(&f.source, &f.target, SHARED_ITEMS).unwrap();
        assert!(again
            .iter()
            .all(|r| r.outcome == LinkOutcome::AlreadyLinked));
    }

    #[test]
    fn moves_an_existing_claude_md_and_skills_aside_instead_of_deleting_them() {
        let f = fixture();
        fs::create_dir_all(f.target.join("skills").join("old")).unwrap();
        fs::write(f.target.join("CLAUDE.md"), "# work only\n").unwrap();
        let res = link_shared(&f.source, &f.target, SHARED_ITEMS).unwrap();
        assert!(res.iter().all(|r| r.outcome == LinkOutcome::Linked));
        let backed: Vec<_> = res
            .iter()
            .filter(|r| r.backup.is_some())
            .map(|r| r.item.as_str())
            .collect();
        assert_eq!(backed, ["CLAUDE.md", "skills"]);
        assert_eq!(
            res[0].backup.as_deref(),
            Some(f.target.join("CLAUDE.md.wraithgrid-backup").as_path())
        );
        assert_eq!(
            read(&f.target.join("CLAUDE.md.wraithgrid-backup")),
            "# work only\n"
        );
        assert!(f
            .target
            .join("skills.wraithgrid-backup")
            .join("old")
            .exists());
    }

    #[test]
    fn a_second_backup_gets_a_timestamp() {
        let f = fixture();
        fs::create_dir_all(&f.target).unwrap();
        fs::write(f.target.join("CLAUDE.md"), "# new\n").unwrap();
        fs::write(f.target.join("CLAUDE.md.wraithgrid-backup"), "# old\n").unwrap();
        let at = UNIX_EPOCH + Duration::from_millis(1_794_477_000_123);
        let res = link_shared_at(&f.source, &f.target, &SHARED_ITEMS[..1], &|| at).unwrap();
        let backup = f
            .target
            .join("CLAUDE.md.wraithgrid-backup-2026-11-12T09-50-00-123Z");
        assert_eq!(res[0].backup.as_deref(), Some(backup.as_path()));
        assert_eq!(read(&backup), "# new\n");
        assert_eq!(
            read(&f.target.join("CLAUDE.md.wraithgrid-backup")),
            "# old\n"
        );
    }

    #[test]
    fn replaces_a_link_that_points_somewhere_else_without_a_backup() {
        let f = fixture();
        fs::create_dir_all(&f.target).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", f.target.join("CLAUDE.md")).unwrap();
        let res = link_shared(&f.source, &f.target, SHARED_ITEMS).unwrap();
        assert_eq!(res[0], ItemResult::new("CLAUDE.md", LinkOutcome::Linked));
        assert_eq!(
            fs::read_link(f.target.join("CLAUDE.md")).unwrap(),
            f.source.join("CLAUDE.md")
        );
        assert!(!exists_no_follow(
            &f.target.join("CLAUDE.md.wraithgrid-backup")
        ));
    }

    #[test]
    fn does_nothing_when_the_account_is_the_source() {
        let f = fixture();
        assert!(link_shared(&f.source, &f.source, SHARED_ITEMS)
            .unwrap()
            .is_empty());
        assert!(unlink_shared(&f.source, &f.source, SHARED_ITEMS).is_empty());
    }

    #[test]
    fn unlinking_removes_only_its_own_links_and_restores_backups() {
        let f = fixture();
        fs::create_dir_all(&f.target).unwrap();
        fs::write(f.target.join("CLAUDE.md"), "# work only\n").unwrap();
        link_shared(&f.source, &f.target, SHARED_ITEMS).unwrap();
        let res = unlink_shared(&f.source, &f.target, SHARED_ITEMS);
        let want: Vec<_> = SHARED_ITEMS
            .iter()
            .map(|i| {
                let o = if i.name == "CLAUDE.md" {
                    LinkOutcome::Restored
                } else {
                    LinkOutcome::Unlinked
                };
                (i.name, o)
            })
            .collect();
        assert_eq!(outcomes(&res), want);
        assert_eq!(read(&f.target.join("CLAUDE.md")), "# work only\n");
        assert!(!exists_no_follow(&f.target.join("skills")));
        // The source itself is untouched.
        assert_eq!(read(&f.source.join("CLAUDE.md")), "# shared rules\n");
    }

    #[test]
    fn never_touches_real_files_that_are_not_its_links() {
        let f = fixture();
        fs::create_dir_all(&f.target).unwrap();
        fs::write(f.target.join("CLAUDE.md"), "# mine\n").unwrap();
        assert!(unlink_shared(&f.source, &f.target, SHARED_ITEMS).is_empty());
        assert_eq!(read(&f.target.join("CLAUDE.md")), "# mine\n");
    }

    #[test]
    fn creates_empty_shared_items_without_overwriting() {
        let f = fixture();
        let fresh = f.root.join("fresh");
        ensure_source(&fresh, SHARED_ITEMS).unwrap();
        assert_eq!(read(&fresh.join("CLAUDE.md")), "");
        assert_eq!(read(&fresh.join("settings.json")), "{}\n");
        assert!(fresh.join("skills").is_dir());
        assert!(fresh.join("plugins").is_dir());
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(fresh.join("CLAUDE.md"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
        ensure_source(&f.source, SHARED_ITEMS).unwrap();
        assert_eq!(read(&f.source.join("CLAUDE.md")), "# shared rules\n");
    }

    #[test]
    fn deleting_an_account_dir_removes_the_links_but_not_the_shared_files() {
        let f = fixture();
        link_shared(&f.source, &f.target, SHARED_ITEMS).unwrap();
        fs::remove_dir_all(&f.target).unwrap();
        assert_eq!(read(&f.source.join("CLAUDE.md")), "# shared rules\n");
        assert!(f.source.join("skills").join("deploy").exists());
    }

    #[test]
    fn seeds_the_shared_dir_once_from_the_cli_home_and_never_changes_the_home() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        fs::create_dir_all(home.join(".claude").join("skills").join("deploy")).unwrap();
        fs::write(home.join(".claude").join("CLAUDE.md"), "# rules\n").unwrap();
        std::os::unix::fs::symlink("deploy", home.join(".claude/skills/alias")).unwrap();
        let spec = share_spec("claude").unwrap();
        let dir = prepare_shared(home, "claude", spec).unwrap();
        assert_eq!(dir, shared_dir(home, "claude"));
        assert_eq!(dir, home.join(".wraithgrid/shared/claude"));
        assert_eq!(read(&dir.join("CLAUDE.md")), "# rules\n");
        assert!(dir.join("skills").join("deploy").exists());
        // Links are copied as links, their target text unchanged.
        assert_eq!(
            fs::read_link(dir.join("skills/alias")).unwrap(),
            PathBuf::from("deploy")
        );
        assert!(dir.join("plugins").exists());
        // Later changes to the home don't overwrite the shared copy.
        fs::write(home.join(".claude").join("CLAUDE.md"), "# changed\n").unwrap();
        prepare_shared(home, "claude", spec).unwrap();
        assert_eq!(read(&dir.join("CLAUDE.md")), "# rules\n");
        let st = fs::symlink_metadata(home.join(".claude").join("CLAUDE.md")).unwrap();
        assert!(!st.file_type().is_symlink());
    }

    #[test]
    fn gives_codex_accounts_agents_md_skills_and_prompts_not_claude_files() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        fs::create_dir_all(home.join(".codex").join("skills").join("review")).unwrap();
        let spec = share_spec("codex").unwrap();
        let dir = prepare_shared(home, "codex", spec).unwrap();
        let acc = home.join("acc");
        link_shared(&dir, &acc, spec.items).unwrap();
        assert_eq!(
            fs::read_link(acc.join("AGENTS.md")).unwrap(),
            dir.join("AGENTS.md")
        );
        assert!(acc.join("skills").join("review").exists());
        assert!(acc.join("prompts").exists());
        assert!(!acc.join("CLAUDE.md").exists());
        unlink_shared(&dir, &acc, spec.items);
        assert!(!exists_no_follow(&acc.join("AGENTS.md")));
    }

    fn account(id: &str, cli: &str, config_dir: &str) -> Account {
        Account {
            id: id.into(),
            name: format!("{id}-name"),
            cli: cli.into(),
            config_dir: config_dir.into(),
        }
    }

    #[test]
    fn apply_links_every_account_and_per_account_takes_the_links_out() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let work = home.join(".wraithgrid/accounts/work");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("CLAUDE.md"), "# work\n").unwrap();
        let accounts = [
            account("w", "claude", "~/.wraithgrid/accounts/work"),
            account("g", "gemini", "~/.wraithgrid/accounts/gem"),
            account("x", "cursor", "~/.wraithgrid/accounts/cur"),
        ];
        let logs = RefCell::new(Vec::new());
        let log = |l: &str| logs.borrow_mut().push(l.to_string());
        let brains = SharedBrains::new(home);

        let report = brains.apply(SharedMode::Overall, &accounts, &log).unwrap();
        assert_eq!(report.len(), SHARED_ITEMS.len() + 3);
        assert_eq!(report[0].account, "w-name");
        assert_eq!(
            report[0].result.backup,
            Some(work.join("CLAUDE.md.wraithgrid-backup"))
        );
        // gemini reads its items from a .gemini folder in the config dir.
        let gem = home.join(".wraithgrid/accounts/gem/.gemini");
        assert_eq!(
            fs::read_link(gem.join("GEMINI.md")).unwrap(),
            home.join(".wraithgrid/shared/gemini/GEMINI.md")
        );
        assert!(!home.join(".wraithgrid/accounts/cur").exists());
        assert_eq!(
            logs.borrow().last().unwrap(),
            "sharing one brain per provider with 3 accounts"
        );

        let report = brains
            .apply(SharedMode::PerAccount, &accounts, &log)
            .unwrap();
        assert_eq!(report[0].result.outcome, LinkOutcome::Restored);
        assert_eq!(read(&work.join("CLAUDE.md")), "# work\n");
        assert!(!exists_no_follow(&gem.join("GEMINI.md")));
        assert_eq!(
            logs.borrow().last().unwrap(),
            "stopped sharing instructions and skills"
        );
    }

    #[test]
    fn unshare_takes_out_new_and_legacy_links_before_a_delete() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let acc = home.join(".wraithgrid/accounts/old");
        let legacy = home.join(".claude");
        ensure_source(&legacy, SHARED_ITEMS).unwrap();
        fs::write(legacy.join("CLAUDE.md"), "# legacy\n").unwrap();
        // An account from before the shared dir links straight to ~/.claude.
        link_shared(&legacy, &acc, &SHARED_ITEMS[..2]).unwrap();
        let brains = SharedBrains::new(home);
        brains.sync_shared(
            SharedMode::Overall,
            &account("o", "claude", &acc.to_string_lossy()),
            &|l| panic!("{l}"),
        );
        // The shared dir was seeded from ~/.claude, and the old links now point there.
        let shared = shared_dir(home, "claude");
        assert_eq!(
            fs::read_link(acc.join("CLAUDE.md")).unwrap(),
            shared.join("CLAUDE.md")
        );
        assert_eq!(read(&shared.join("CLAUDE.md")), "# legacy\n");

        let real = fs::canonicalize(&acc).unwrap();
        let res = brains.unshare("claude", &real.to_string_lossy());
        assert_eq!(res.len(), SHARED_ITEMS.len());
        assert!(res.iter().all(|r| r.outcome == LinkOutcome::Unlinked));
        fs::remove_dir_all(&acc).unwrap();
        assert_eq!(read(&shared.join("CLAUDE.md")), "# legacy\n");
        assert_eq!(read(&legacy.join("CLAUDE.md")), "# legacy\n");
        assert!(brains.unshare("cursor", "~/x").is_empty());
    }

    #[test]
    fn sync_does_nothing_per_account_and_logs_link_failures() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let brains = SharedBrains::new(home);
        let acc = account("w", "claude", "~/w");
        brains.sync_shared(SharedMode::PerAccount, &acc, &|l| panic!("{l}"));
        assert!(!home.join("w").exists());

        // A config dir that is a file cannot hold links.
        fs::write(home.join("f"), "").unwrap();
        let logs = RefCell::new(Vec::new());
        let log = |l: &str| logs.borrow_mut().push(l.to_string());
        brains.sync_shared(SharedMode::Overall, &account("f", "claude", "~/f"), &log);
        assert_eq!(logs.borrow().len(), 1);
        assert!(
            logs.borrow()[0].starts_with("could not share instructions and skills with f-name: ")
        );
        assert_eq!(
            brains.apply(SharedMode::Overall, &[account("f", "claude", "~/f")], &log),
            Err(SHARED_APPLY_FAILED.to_string())
        );
        assert!(logs
            .borrow()
            .last()
            .unwrap()
            .starts_with("shared:apply failed: "));
    }
}
