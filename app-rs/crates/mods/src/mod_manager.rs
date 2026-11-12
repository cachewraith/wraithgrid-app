//! The Mods view's actions over every claude account (port of the mod handlers of
//! `src/main/ipc.ts`): list, switch, browse, install, uninstall/update and manage
//! marketplaces, each through the account's own claude. It remembers what it last listed
//! and offered, so an action can only name a row the user was shown.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::thread;

use serde::Serialize;

use crate::mod_community::{CommunityCatalog, CommunityMods};
use crate::mods::{
    list_marketplaces, list_mod_catalog, list_mods, marketplace_add_args, marketplace_of,
    marketplace_remove_args, merge_catalogs, mod_change_args, mod_install_args, mod_toggle_args,
    with_mod_links, CatalogMod, ClaudeRunner, Env, Marketplace, MarketplaceInfo, ModInfo,
    RunOptions, INSTALL_TIMEOUT, TIMEOUT,
};
use crate::util::{contract_home, js_trim};
use crate::Account;

/// The text when there is no claude binary to run.
pub const CLAUDE_NOT_FOUND: &str = "claude was not found on PATH. Set its path in Settings.";
const NO_LONGER_LISTED: &str = "That mod is no longer listed. Refresh the list.";
const NO_LONGER_OFFERED: &str = "That mod is no longer offered. Refresh the list.";
const MODS_PARALLEL: usize = 4;

/// What each action needs from the app, fresh each call.
#[derive(Clone, Copy)]
pub struct ModDeps<'a> {
    /// Every account; only claude ones have mods.
    pub accounts: &'a [Account],
    /// The claude binary (`ClaudeCli` at the resolved path), or None when it was not found.
    pub claude: Option<&'a dyn ClaudeRunner>,
    /// The env an account's claude runs with (`buildPaneEnv` for its config dir).
    pub env_for: &'a (dyn Fn(&Account) -> Env + Sync),
    pub log: &'a dyn Fn(&str),
}

impl<'a> ModDeps<'a> {
    fn claude_accounts(&self) -> Vec<&'a Account> {
        self.accounts.iter().filter(|a| a.cli == "claude").collect()
    }

    fn runner(&self) -> Result<&'a dyn ClaudeRunner, String> {
        self.claude.ok_or_else(|| CLAUDE_NOT_FOUND.to_string())
    }
}

/// One account's installed mods, or why claude could not list them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountMods {
    pub account_id: String,
    pub mods: Result<Vec<ModInfo>, String>,
}

/// An account claude refused a change for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FailedAccount {
    pub account: String,
    pub error: String,
}

/// The marketplaces merged over the claude accounts, out of `total` accounts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MarketplaceList {
    pub marketplaces: Vec<MarketplaceInfo>,
    pub total: usize,
}

/// What `mods.change` does to a listed mod.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModChange {
    Uninstall,
    Update,
}

#[derive(Default)]
struct State {
    /// What each account's claude last listed; a toggle must name a row from here.
    listed: Vec<(String, Vec<ModInfo>)>,
    /// The marketplaces each account's claude last listed: mod links, and what an install
    /// adds. In first-listed order, as a JS Map keeps it.
    marketplaces: Vec<(String, Vec<Marketplace>)>,
    /// The ids the last catalog offered; an install must name one of them or a listed mod.
    offered: HashSet<String>,
    /// The same for the community list, with the repository each mod's marketplace comes from.
    community: CommunityMods,
}

fn get<'s, T>(map: &'s [(String, T)], key: &str) -> Option<&'s T> {
    map.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn set<T>(map: &mut Vec<(String, T)>, key: &str, value: T) {
    match map.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value,
        None => map.push((key.to_string(), value)),
    }
}

/// Runs `f` for each item, `size` at a time in parallel, keeping their order.
fn batched<T: Sync, R: Send>(items: &[T], size: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let mut out = Vec::new();
    for batch in items.chunks(size.max(1)) {
        thread::scope(|s| {
            let running: Vec<_> = batch.iter().map(|x| s.spawn(|| f(x))).collect();
            out.extend(running.into_iter().map(|h| h.join().expect("no panic")));
        });
    }
    out
}

/// The Mods view's state and actions; one per app.
pub struct ModManager {
    home_dir: PathBuf,
    community: CommunityCatalog,
    state: Mutex<State>,
}

impl ModManager {
    pub fn new(home_dir: impl Into<PathBuf>, community: CommunityCatalog) -> Self {
        ModManager {
            home_dir: home_dir.into(),
            community,
            state: Mutex::new(State::default()),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Links are an extra: an account whose claude cannot list marketplaces still lists its
    /// mods. Not remembered here: the caller does, in account order.
    fn read_marketplaces(&self, run: &dyn ClaudeRunner, env: &Env) -> Vec<Marketplace> {
        list_marketplaces(run, env, &self.home_dir).unwrap_or_default()
    }

    fn remember_marketplaces(&self, acc: &Account, found: &[Marketplace]) {
        set(&mut self.state().marketplaces, &acc.id, found.to_vec());
    }

    /// Asks each claude account's CLI for its installed mods.
    pub fn list(&self, deps: &ModDeps<'_>) -> Vec<AccountMods> {
        let accounts = deps.claude_accounts();
        let run = match deps.runner() {
            Ok(run) => run,
            Err(e) => {
                return accounts
                    .iter()
                    .map(|a| AccountMods {
                        account_id: a.id.clone(),
                        mods: Err(e.clone()),
                    })
                    .collect()
            }
        };
        let env_for = deps.env_for;
        let read = batched(&accounts, MODS_PARALLEL, |acc| {
            let env = env_for(acc);
            thread::scope(|s| {
                let markets = s.spawn(|| self.read_marketplaces(run, &env));
                let plain = list_mods(run, &env, &self.home_dir);
                (markets.join().expect("no panic"), plain)
            })
        });
        let mut out = Vec::new();
        for (acc, (markets, plain)) in accounts.iter().zip(read) {
            self.remember_marketplaces(acc, &markets);
            let mods = plain.map(|plain| with_mod_links(plain, &markets));
            let mut st = self.state();
            match &mods {
                Ok(mods) => set(&mut st.listed, &acc.id, mods.clone()),
                Err(_) => st.listed.retain(|(k, _)| k != &acc.id),
            }
            out.push(AccountMods {
                account_id: acc.id.clone(),
                mods,
            });
        }
        out
    }

    /// Enables or disables one listed mod through the account's CLI.
    pub fn set_enabled(
        &self,
        deps: &ModDeps<'_>,
        account_id: &str,
        id: &str,
        scope: &str,
        project_path: Option<&str>,
        enabled: bool,
    ) -> Result<(), String> {
        let account = deps
            .accounts
            .iter()
            .find(|x| x.id == account_id && x.cli == "claude");
        let row = get(&self.state().listed, account_id).and_then(|l| {
            l.iter()
                .find(|m| {
                    m.id == id && m.scope == scope && m.project_path.as_deref() == project_path
                })
                .cloned()
        });
        let (Some(account), Some(row)) = (account, row) else {
            return Err(NO_LONGER_LISTED.into());
        };
        let args = mod_toggle_args(&row.id, &row.scope, enabled)
            .ok_or("That mod cannot be changed from here.")?;
        let run = deps.runner()?;
        // A project-scoped mod is switched from its own project folder, as claude lists it.
        let mut cwd = self.home_dir.clone();
        if let Some(p) = &row.project_path {
            match std::fs::metadata(p) {
                Ok(m) if m.is_dir() => cwd = PathBuf::from(p),
                Ok(_) => {}
                Err(_) => {
                    return Err(format!(
                        "Project folder not found: {}",
                        contract_home(Path::new(p), &self.home_dir)
                    ))
                }
            }
        }
        let env = (deps.env_for)(account);
        let opts = RunOptions {
            env: &env,
            cwd: &cwd,
            timeout: TIMEOUT,
        };
        run.run(&args, &opts)?;
        let did = if enabled { "enabled" } else { "disabled" };
        (deps.log)(&format!("{did} mod {} for {}", row.id, account.name));
        Ok(())
    }

    /// Every mod the marketplaces of all claude accounts offer, merged by id.
    pub fn catalog(&self, deps: &ModDeps<'_>) -> Result<Vec<CatalogMod>, String> {
        let accounts = deps.claude_accounts();
        if accounts.is_empty() {
            return Err("No claude account yet.".into());
        }
        let run = deps.runner()?;
        // Every account's marketplaces, merged: an install adds the marketplace where missing.
        let env_for = deps.env_for;
        let read = batched(&accounts, MODS_PARALLEL, |acc| {
            let env = env_for(acc);
            let markets = self.read_marketplaces(run, &env);
            let list = list_mod_catalog(run, &env, &self.home_dir, &markets);
            (markets, list)
        });
        let mut lists = Vec::new();
        let mut error = String::new();
        for (acc, (markets, list)) in accounts.iter().zip(read) {
            self.remember_marketplaces(acc, &markets);
            match list {
                Ok(list) => lists.push(list),
                Err(e) if error.is_empty() => error = e,
                Err(_) => {}
            }
        }
        if lists.is_empty() {
            return Err(error);
        }
        let mods = merge_catalogs(lists);
        self.state().offered = mods.iter().map(|m| m.id.clone()).collect();
        Ok(mods)
    }

    /// The mods claudemod.com lists that can be installed; contacts that site and GitHub.
    pub fn community(&self) -> Result<Vec<CatalogMod>, String> {
        let community = self.community.read()?;
        let mods = community.mods.clone();
        self.state().community = community;
        Ok(mods)
    }

    /// Installs a mod for every claude account that lacks it (user scope), each through its
    /// CLI; Ok names the accounts claude refused it for.
    pub fn install(&self, deps: &ModDeps<'_>, id: &str) -> Result<Vec<FailedAccount>, String> {
        let args = mod_install_args(id);
        let accounts = deps.claude_accounts();
        let (lacking, source) = {
            let st = self.state();
            let has = |account_id: &str| {
                get(&st.listed, account_id)
                    .is_some_and(|l| l.iter().any(|m| m.id == id && m.scope == "user"))
            };
            let offered = st.offered.contains(id)
                || st
                    .community
                    .mods
                    .iter()
                    .any(|m| m.id == id && m.clone.is_none());
            if args.is_none() || !(offered || accounts.iter().any(|x| has(&x.id))) {
                return Err(NO_LONGER_OFFERED.into());
            }
            // A marketplace an account already has wins over the community list's one of
            // that name.
            let market = marketplace_of(id);
            let source = st
                .marketplaces
                .iter()
                .flat_map(|(_, l)| l)
                .chain(&st.community.marketplaces)
                .find(|m| m.name == market && m.add.is_some())
                .cloned();
            let lacking: Vec<&Account> = accounts.into_iter().filter(|x| !has(&x.id)).collect();
            (lacking, source)
        };
        let args = args.expect("checked");
        let run = deps.runner()?;
        let market = marketplace_of(id);
        let add_args = source
            .as_ref()
            .and_then(|s| s.add.as_deref())
            .and_then(marketplace_add_args);
        let mut failed = Vec::new();
        // One account at a time: accounts that share ~/.claude write the same plugin files.
        for account in lacking {
            let env = (deps.env_for)(account);
            let opts = RunOptions {
                env: &env,
                cwd: &self.home_dir,
                timeout: INSTALL_TIMEOUT,
            };
            let step = || -> Result<(), String> {
                let known = get(&self.state().marketplaces, &account.id).cloned();
                if let (Some(add), Some(known)) = (&add_args, known) {
                    if !known.iter().any(|m| m.name == market) {
                        run.run(add, &opts)?;
                        let mut with = known;
                        with.extend(source.clone());
                        set(&mut self.state().marketplaces, &account.id, with);
                        (deps.log)(&format!("added marketplace {market} for {}", account.name));
                    }
                }
                run.run(&args, &opts)?;
                (deps.log)(&format!("installed mod {id} for {}", account.name));
                Ok(())
            };
            if let Err(error) = step() {
                failed.push(FailedAccount {
                    account: account.name.clone(),
                    error,
                });
            }
        }
        Ok(failed)
    }

    /// Runs a step for each account in turn, collecting the ones claude refused.
    fn for_each_account(
        &self,
        deps: &ModDeps<'_>,
        accounts: &[&Account],
        step_for: impl Fn(&Account) -> Option<(Vec<String>, PathBuf)>,
        what: &str,
    ) -> Result<Vec<FailedAccount>, String> {
        let run = deps.runner()?;
        let mut failed = Vec::new();
        // One account at a time: accounts that share ~/.claude write the same plugin files.
        for &account in accounts {
            let Some((args, cwd)) = step_for(account) else {
                continue;
            };
            let env = (deps.env_for)(account);
            let opts = RunOptions {
                env: &env,
                cwd: &cwd,
                timeout: INSTALL_TIMEOUT,
            };
            match run.run(&args, &opts) {
                Ok(_) => (deps.log)(&format!("{what} for {}", account.name)),
                Err(error) => failed.push(FailedAccount {
                    account: account.name.clone(),
                    error,
                }),
            }
        }
        Ok(failed)
    }

    /// Uninstalls or updates a listed mod in every claude account that has it.
    pub fn change(
        &self,
        deps: &ModDeps<'_>,
        id: &str,
        change: ModChange,
    ) -> Result<Vec<FailedAccount>, String> {
        // Only rows the last list showed: a project-scoped mod changes from its own folder.
        let rows: Vec<(&Account, ModInfo)> = {
            let st = self.state();
            deps.claude_accounts()
                .into_iter()
                .filter_map(|x| {
                    let row = get(&st.listed, &x.id)?.iter().find(|m| m.id == id)?;
                    Some((x, row.clone()))
                })
                .collect()
        };
        if rows.is_empty() {
            return Err(NO_LONGER_LISTED.into());
        }
        let accounts: Vec<&Account> = rows.iter().map(|(a, _)| *a).collect();
        let update = change == ModChange::Update;
        self.for_each_account(
            deps,
            &accounts,
            |account| {
                let (_, row) = rows.iter().find(|(a, _)| a.id == account.id)?;
                let args = mod_change_args(&row.id, &row.scope, update)?;
                let cwd = row
                    .project_path
                    .as_ref()
                    .map_or_else(|| self.home_dir.clone(), PathBuf::from);
                Some((args, cwd))
            },
            &format!(
                "{} mod {id}",
                if update { "updated" } else { "uninstalled" }
            ),
        )
    }

    /// The marketplaces of all claude accounts, merged by name.
    pub fn marketplaces(&self, deps: &ModDeps<'_>) -> Result<MarketplaceList, String> {
        let accounts = deps.claude_accounts();
        let run = deps.runner()?;
        let env_for = deps.env_for;
        let lists = batched(&accounts, accounts.len(), |acc| {
            self.read_marketplaces(run, &env_for(acc))
        });
        let mut merged: Vec<MarketplaceInfo> = Vec::new();
        for (acc, list) in accounts.iter().zip(&lists) {
            self.remember_marketplaces(acc, list);
        }
        for m in lists.into_iter().flatten() {
            match merged.iter_mut().find(|x| x.name == m.name) {
                Some(had) => {
                    had.url = had.url.take().or(m.url);
                    had.accounts += 1;
                }
                None => merged.push(MarketplaceInfo {
                    name: m.name,
                    url: m.url,
                    accounts: 1,
                }),
            }
        }
        Ok(MarketplaceList {
            marketplaces: merged,
            total: accounts.len(),
        })
    }

    /// Adds a marketplace (`owner/repo` or an https address) to every claude account.
    pub fn add_marketplace(
        &self,
        deps: &ModDeps<'_>,
        source: &str,
    ) -> Result<Vec<FailedAccount>, String> {
        let source = js_trim(source);
        let args = marketplace_add_args(source)
            .ok_or("Give a GitHub repository (owner/repo) or an https address.")?;
        self.for_each_account(
            deps,
            &deps.claude_accounts(),
            |_| Some((args.clone(), self.home_dir.clone())),
            &format!("added marketplace {source}"),
        )
    }

    /// Removes a marketplace from every account that has it.
    pub fn remove_marketplace(
        &self,
        deps: &ModDeps<'_>,
        name: &str,
    ) -> Result<Vec<FailedAccount>, String> {
        let args = marketplace_remove_args(name).ok_or("Not a marketplace name.")?;
        let accounts: Vec<&Account> = {
            let st = self.state();
            deps.claude_accounts()
                .into_iter()
                .filter(|x| {
                    get(&st.marketplaces, &x.id).is_some_and(|l| l.iter().any(|m| m.name == name))
                })
                .collect()
        };
        if accounts.is_empty() {
            return Err("No account has that marketplace. Refresh the list.".into());
        }
        let r = self.for_each_account(
            deps,
            &accounts,
            |_| Some((args.clone(), self.home_dir.clone())),
            &format!("removed marketplace {name}"),
        );
        let mut st = self.state();
        for x in &accounts {
            let kept = get(&st.marketplaces, &x.id)
                .map(|l| l.iter().filter(|m| m.name != name).cloned().collect())
                .unwrap_or_default();
            set(&mut st.marketplaces, &x.id, kept);
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mod_community::{FetchRequest, FetchResponse};
    use serde_json::json;
    use std::cell::RefCell;

    type Call = (String, Vec<String>, PathBuf);
    /// What the fake claude prints for an account and a command line.
    type Answer = fn(&str, &str) -> Result<String, String>;

    /// A fake claude: answers by account (CLAUDE_CONFIG_DIR) and command, records each call.
    struct FakeClaude {
        calls: Mutex<Vec<Call>>,
        answer: Answer,
    }

    impl FakeClaude {
        fn new(answer: Answer) -> Self {
            FakeClaude {
                calls: Mutex::new(Vec::new()),
                answer,
            }
        }

        fn calls(&self) -> Vec<(String, String)> {
            let mut c: Vec<_> = self
                .calls
                .lock()
                .unwrap()
                .iter()
                .map(|(acc, args, _)| (acc.clone(), args.join(" ")))
                .collect();
            c.sort();
            c
        }
    }

    impl ClaudeRunner for FakeClaude {
        fn run(&self, args: &[String], opts: &RunOptions<'_>) -> Result<String, String> {
            let acc = opts.env["CLAUDE_CONFIG_DIR"].clone();
            let line = args.join(" ");
            self.calls
                .lock()
                .unwrap()
                .push((acc.clone(), args.to_vec(), opts.cwd.to_path_buf()));
            (self.answer)(&acc, &line)
        }
    }

    fn account(id: &str, cli: &str) -> Account {
        Account {
            id: id.into(),
            name: format!("{id}-name"),
            cli: cli.into(),
            config_dir: format!("~/.wraithgrid/accounts/{id}"),
        }
    }

    fn env_for(acc: &Account) -> Env {
        Env::from([("CLAUDE_CONFIG_DIR".to_string(), acc.id.clone())])
    }

    fn manager(home: &Path) -> ModManager {
        let community = CommunityCatalog::new(|req: &FetchRequest<'_>| {
            let body = if req.url.ends_with("browse?category=mod") {
                let card = json!({ "slug": "burn", "githubUrl": "https://github.com/o/burn" });
                format!(
                    "<script>[{}]</script>",
                    card.to_string().replace('"', "\\\"")
                )
            } else {
                json!({ "name": "burnmkt", "plugins": [{ "name": "burn" }] }).to_string()
            };
            Ok(FetchResponse { status: 200, body })
        });
        ModManager::new(home, community)
    }

    fn accounts() -> Vec<Account> {
        vec![
            account("a", "claude"),
            account("b", "claude"),
            account("c", "codex"),
        ]
    }

    /// Account a has tally (user) and the acme marketplace; b has nothing; both list clock.
    fn answer(acc: &str, line: &str) -> Result<String, String> {
        Ok(match (acc, line) {
            ("a", "plugin list --json") => json!([
                { "id": "tally@acme", "version": "1", "scope": "user", "enabled": true },
                { "id": "proj@acme", "scope": "project", "enabled": true, "projectPath": "/nowhere/p" }
            ])
            .to_string(),
            (_, "plugin list --json") => "[]".into(),
            ("a", "plugin marketplace list --json") => {
                json!([{ "name": "acme", "source": "github", "repo": "acme/mods" }]).to_string()
            }
            (_, "plugin marketplace list --json") => "[]".into(),
            (_, "plugin list --available --json") => json!({ "available": [
                { "pluginId": "clock@acme", "name": "clock", "marketplaceName": "acme", "source": "./clock" }
            ]})
            .to_string(),
            ("b", l) if l.starts_with("plugin install") => return Err("not signed in".into()),
            _ => String::new(),
        })
    }

    #[test]
    fn lists_each_claude_account_and_switches_only_listed_rows() {
        let home = tempfile::tempdir().unwrap();
        let mgr = manager(home.path());
        let claude = FakeClaude::new(answer);
        let accs = accounts();
        let logs = RefCell::new(Vec::new());
        let log = |l: &str| logs.borrow_mut().push(l.to_string());
        let deps = ModDeps {
            accounts: &accs,
            claude: Some(&claude),
            env_for: &env_for,
            log: &log,
        };
        assert_eq!(
            mgr.set_enabled(&deps, "a", "tally@acme", "user", None, false),
            Err(NO_LONGER_LISTED.into())
        );
        let listed = mgr.list(&deps);
        assert_eq!(listed.len(), 2);
        let a = listed[0].mods.as_ref().unwrap();
        assert_eq!(a[0].url.as_deref(), Some("https://github.com/acme/mods"));
        assert_eq!(listed[1].mods, Ok(vec![]));

        mgr.set_enabled(&deps, "a", "tally@acme", "user", None, false)
            .unwrap();
        assert_eq!(
            logs.borrow().last().unwrap(),
            "disabled mod tally@acme for a-name"
        );
        assert!(claude
            .calls()
            .contains(&("a".into(), "plugin disable tally@acme --scope user".into())));
        assert_eq!(
            mgr.set_enabled(&deps, "a", "proj@acme", "project", Some("/nowhere/p"), true),
            Err("Project folder not found: /nowhere/p".into())
        );
        assert_eq!(
            mgr.set_enabled(&deps, "a", "tally@acme", "project", None, false),
            Err(NO_LONGER_LISTED.into())
        );
    }

    #[test]
    fn without_claude_every_action_says_so() {
        let home = tempfile::tempdir().unwrap();
        let mgr = manager(home.path());
        let accs = accounts();
        let deps = ModDeps {
            accounts: &accs,
            claude: None,
            env_for: &env_for,
            log: &|_| {},
        };
        let listed = mgr.list(&deps);
        assert_eq!(listed.len(), 2);
        assert!(listed
            .iter()
            .all(|r| r.mods == Err(CLAUDE_NOT_FOUND.into())));
        assert_eq!(mgr.catalog(&deps), Err(CLAUDE_NOT_FOUND.into()));
        assert_eq!(
            mgr.add_marketplace(&deps, " acme/mods "),
            Err(CLAUDE_NOT_FOUND.into())
        );
        let none = ModDeps {
            accounts: &[],
            ..deps
        };
        assert_eq!(mgr.catalog(&none), Err("No claude account yet.".into()));
    }

    #[test]
    fn installs_offered_mods_where_missing_adding_the_marketplace_first() {
        let home = tempfile::tempdir().unwrap();
        let mgr = manager(home.path());
        let claude = FakeClaude::new(answer);
        let accs = accounts();
        let logs = RefCell::new(Vec::new());
        let log = |l: &str| logs.borrow_mut().push(l.to_string());
        let deps = ModDeps {
            accounts: &accs,
            claude: Some(&claude),
            env_for: &env_for,
            log: &log,
        };
        assert_eq!(
            mgr.install(&deps, "clock@acme"),
            Err(NO_LONGER_OFFERED.into())
        );
        mgr.list(&deps);
        let catalog = mgr.catalog(&deps).unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(
            catalog[0].url.as_deref(),
            Some("https://github.com/acme/mods/tree/HEAD/clock")
        );
        assert_eq!(mgr.install(&deps, "--yes"), Err(NO_LONGER_OFFERED.into()));

        claude.calls.lock().unwrap().clear();
        let failed = mgr.install(&deps, "clock@acme").unwrap();
        assert_eq!(
            failed,
            [FailedAccount {
                account: "b-name".into(),
                error: "not signed in".into()
            }]
        );
        assert_eq!(
            claude.calls(),
            [
                ("a".into(), "plugin install clock@acme --scope user".into()),
                ("b".into(), "plugin install clock@acme --scope user".into()),
                ("b".into(), "plugin marketplace add acme/mods".into()),
            ]
        );
        assert!(logs
            .borrow()
            .contains(&"added marketplace acme for b-name".to_string()));
        assert!(logs
            .borrow()
            .contains(&"installed mod clock@acme for a-name".to_string()));

        // An account that already has it at user scope is left alone.
        claude.calls.lock().unwrap().clear();
        mgr.install(&deps, "tally@acme").unwrap();
        let calls = claude.calls();
        assert!(calls.iter().all(|(acc, _)| acc == "b"), "{calls:?}");
    }

    #[test]
    fn installs_a_community_mod_from_its_repository() {
        let home = tempfile::tempdir().unwrap();
        let mgr = manager(home.path());
        let claude = FakeClaude::new(answer);
        let accs = vec![account("a", "claude")];
        let deps = ModDeps {
            accounts: &accs,
            claude: Some(&claude),
            env_for: &env_for,
            log: &|_| {},
        };
        mgr.list(&deps);
        let community = mgr.community().unwrap();
        assert_eq!(community[0].id, "burn@burnmkt");
        assert_eq!(mgr.install(&deps, "burn@burnmkt"), Ok(vec![]));
        let calls = claude.calls();
        assert!(calls.contains(&("a".into(), "plugin marketplace add o/burn".into())));
        assert!(calls.contains(&(
            "a".into(),
            "plugin install burn@burnmkt --scope user".into()
        )));
    }

    #[test]
    fn changes_and_marketplaces_go_to_the_accounts_that_have_them() {
        let home = tempfile::tempdir().unwrap();
        let mgr = manager(home.path());
        let claude = FakeClaude::new(answer);
        let accs = accounts();
        let logs = RefCell::new(Vec::new());
        let log = |l: &str| logs.borrow_mut().push(l.to_string());
        let deps = ModDeps {
            accounts: &accs,
            claude: Some(&claude),
            env_for: &env_for,
            log: &log,
        };
        assert_eq!(
            mgr.change(&deps, "tally@acme", ModChange::Update),
            Err(NO_LONGER_LISTED.into())
        );
        mgr.list(&deps);
        claude.calls.lock().unwrap().clear();
        assert_eq!(
            mgr.change(&deps, "proj@acme", ModChange::Uninstall),
            Ok(vec![])
        );
        let calls = claude.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1.join(" "),
            "plugin uninstall proj@acme --scope project"
        );
        assert_eq!(calls[0].2, PathBuf::from("/nowhere/p"));
        assert_eq!(
            logs.borrow().last().unwrap(),
            "uninstalled mod proj@acme for a-name"
        );

        let list = mgr.marketplaces(&deps).unwrap();
        assert_eq!(
            list,
            MarketplaceList {
                marketplaces: vec![MarketplaceInfo {
                    name: "acme".into(),
                    url: Some("https://github.com/acme/mods".into()),
                    accounts: 1
                }],
                total: 2
            }
        );
        assert_eq!(
            mgr.add_marketplace(&deps, "/home/me/x"),
            Err("Give a GitHub repository (owner/repo) or an https address.".into())
        );
        assert_eq!(mgr.add_marketplace(&deps, " o/r "), Ok(vec![]));
        assert_eq!(
            logs.borrow().last().unwrap(),
            "added marketplace o/r for b-name"
        );
        assert_eq!(
            mgr.remove_marketplace(&deps, "-rf"),
            Err("Not a marketplace name.".into())
        );
        claude.calls.lock().unwrap().clear();
        assert_eq!(mgr.remove_marketplace(&deps, "acme"), Ok(vec![]));
        assert_eq!(
            claude.calls(),
            [("a".into(), "plugin marketplace remove acme".into())]
        );
        assert_eq!(
            mgr.remove_marketplace(&deps, "acme"),
            Err("No account has that marketplace. Refresh the list.".into())
        );
    }
}
