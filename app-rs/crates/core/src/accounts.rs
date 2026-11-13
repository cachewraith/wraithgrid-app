//! Adding, renaming and removing accounts (`addAccount`, `renameAccount`, `removeAccount` in
//! the Electron store, `account:createDir` in main). New accounts get a config dir under
//! ~/.wraithgrid/accounts named after them, written as `~/…` like the Electron app writes it.

use std::collections::HashSet;
use std::path::Path;

use crate::agent_cli::AgentCli;
use crate::config::{Account, AccountFolder, Config, Extra};
use crate::ids::new_id;
use crate::paths::{
    accounts_root, base_name, contract_home, is_strictly_inside, resolve_user_path, slugify,
};
use crate::types::ACCOUNT_COLORS;

/// The first account color no account uses yet, else one by count.
pub fn next_free_color(accounts: &[Account]) -> String {
    let used: HashSet<String> = accounts.iter().map(|a| a.color.to_lowercase()).collect();
    ACCOUNT_COLORS
        .iter()
        .find(|(_, v)| !used.contains(*v))
        .unwrap_or(&ACCOUNT_COLORS[accounts.len() % ACCOUNT_COLORS.len()])
        .1
        .to_string()
}

pub fn unique_slug(base: &str, taken: &HashSet<String>) -> String {
    let base = if base.is_empty() { "account" } else { base };
    let mut slug = base.to_string();
    let mut i = 2;
    while taken.contains(&slug) {
        slug = format!("{base}-{i}");
        i += 1;
    }
    slug
}

/// Adds an account with its own config dir; the first account also becomes the default.
pub fn add_account(
    config: &mut Config,
    name: &str,
    color: &str,
    cli: AgentCli,
    home: &Path,
) -> Result<String, String> {
    let trimmed: String = name.trim().chars().take(64).collect();
    if trimmed.is_empty() {
        return Err("Enter a name.".into());
    }
    if config
        .accounts
        .iter()
        .any(|a| a.name.to_lowercase() == trimmed.to_lowercase())
    {
        return Err("An account with that name already exists.".into());
    }
    if config.accounts.len() >= 64 {
        return Err("Wraithgrid holds up to 64 accounts.".into());
    }
    let taken: HashSet<String> = config
        .accounts
        .iter()
        .map(|a| base_name(&a.config_dir))
        .collect();
    let slug = unique_slug(&slugify(&trimmed), &taken);
    let dir = accounts_root(home).join(&slug);
    create_private_dir(&dir).map_err(|_| "Could not create the config dir".to_string())?;
    let account = Account {
        id: new_id("a"),
        name: trimmed,
        cli,
        config_dir: contract_home(&dir.to_string_lossy(), &home.to_string_lossy()),
        color: color.into(),
        // gemini and agy ask to sign in on their first launch; the /login banner is claude's.
        signed_in: cli != AgentCli::Claude,
        imported: false,
        icon: String::new(),
        folder_id: None,
        extra: Extra::new(),
    };
    let id = account.id.clone();
    config.accounts.push(account);
    if config.settings.default_account_id.is_none() {
        config.settings.default_account_id = Some(id.clone());
    }
    Ok(id)
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)
}

pub fn rename_account(config: &mut Config, id: &str, name: &str) -> Result<(), String> {
    let trimmed: String = name.trim().chars().take(64).collect();
    if trimmed.is_empty() {
        return Err("Enter a name.".into());
    }
    if config
        .accounts
        .iter()
        .any(|a| a.id != id && a.name.to_lowercase() == trimmed.to_lowercase())
    {
        return Err("An account with that name already exists.".into());
    }
    if let Some(a) = config.accounts.iter_mut().find(|a| a.id == id) {
        a.name = trimmed;
    }
    Ok(())
}

/// Removes an account from the config (its panes are the caller's to stop first). Projects
/// that defaulted to it and the app default forget it; its config dir stays on disk.
pub fn remove_account(config: &mut Config, id: &str) -> Vec<String> {
    let panes: Vec<String> = config
        .workspaces
        .iter()
        .flat_map(|w| &w.panes)
        .filter(|p| p.account_id.as_deref() == Some(id))
        .map(|p| p.id.clone())
        .collect();
    config.accounts.retain(|a| a.id != id);
    for w in &mut config.workspaces {
        if w.account_id.as_deref() == Some(id) {
            w.account_id = None;
        }
    }
    if config.settings.default_account_id.as_deref() == Some(id) {
        config.settings.default_account_id = None;
    }
    panes
}

/// `importAccount`: an existing claude config dir (`~/.claude`) becomes an account where it is;
/// nothing is copied. Named after the folder (`.claude` → `claude`).
pub fn import_account(config: &mut Config, dir: &str) -> Result<String, String> {
    let dir = dir.trim();
    if dir.is_empty() {
        return Err("Choose a folder.".into());
    }
    if config.accounts.iter().any(|a| a.config_dir == dir) {
        return Err("That folder is already an account.".into());
    }
    let base = match slugify(base_name(dir).trim_start_matches('.')) {
        s if s.is_empty() => "imported".to_string(),
        s => s,
    };
    let taken: HashSet<String> = config.accounts.iter().map(|a| a.name.clone()).collect();
    let account = Account {
        id: new_id("a"),
        name: unique_slug(&base, &taken),
        cli: AgentCli::Claude,
        config_dir: dir.to_string(),
        color: next_free_color(&config.accounts),
        signed_in: true,
        imported: true,
        icon: String::new(),
        folder_id: None,
        extra: Extra::new(),
    };
    let id = account.id.clone();
    config.accounts.push(account);
    if config.settings.default_account_id.is_none() {
        config.settings.default_account_id = Some(id.clone());
    }
    Ok(id)
}

/// Where "Also delete its config dir" may delete (`accountDeleteDir`): only a dir a current
/// account points at, and only strictly inside home, also once links are resolved. Ok(None):
/// already gone.
pub fn deletable_config_dir(
    config: &Config,
    dir: &str,
    home: &Path,
) -> Result<Option<std::path::PathBuf>, String> {
    let target = resolve_user_path(dir, home);
    let owned = config
        .accounts
        .iter()
        .any(|a| resolve_user_path(&a.config_dir, home) == target);
    if !owned {
        return Err("That folder does not belong to an account".into());
    }
    if !is_strictly_inside(&target, home) {
        return Err("Refusing to delete a folder outside your home directory".into());
    }
    let Ok(real) = std::fs::canonicalize(&target) else {
        return Ok(None);
    };
    let real_home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if !is_strictly_inside(&real, &real_home) {
        return Err("Refusing to delete a folder outside your home directory".into());
    }
    Ok(Some(real))
}

/// Folders an account list may have (`createFolder`).
pub const FOLDERS_MAX: usize = 32;

/// A folder at the end of the list; None when the name is empty or there are 32 already.
pub fn create_folder(config: &mut Config, name: &str) -> Option<String> {
    let name: String = name.trim().chars().take(64).collect();
    if name.is_empty() || config.account_folders.len() >= FOLDERS_MAX {
        return None;
    }
    let id = new_id("f");
    config.account_folders.push(AccountFolder {
        id: id.clone(),
        name,
        collapsed: false,
        icon: String::new(),
        color: String::new(),
        extra: Extra::default(),
    });
    Some(id)
}

/// Renames a folder; an empty name keeps the old one.
pub fn rename_folder(config: &mut Config, id: &str, name: &str) {
    let name: String = name.trim().chars().take(64).collect();
    if name.is_empty() {
        return;
    }
    for f in config.account_folders.iter_mut().filter(|f| f.id == id) {
        f.name = name.clone();
    }
}

pub fn toggle_folder(config: &mut Config, id: &str) {
    for f in config.account_folders.iter_mut().filter(|f| f.id == id) {
        f.collapsed = !f.collapsed;
    }
}

/// The folder's accounts move to the top level; none are removed.
pub fn delete_folder(config: &mut Config, id: &str) {
    config.account_folders.retain(|f| f.id != id);
    for a in config.accounts.iter_mut() {
        if a.folder_id.as_deref() == Some(id) {
            a.folder_id = None;
        }
    }
}

/// Into a folder, or out of any (None); a folder that doesn't exist changes nothing.
pub fn move_account_to_folder(config: &mut Config, id: &str, folder: Option<&str>) {
    if folder.is_some_and(|f| !config.account_folders.iter().any(|x| x.id == f)) {
        return;
    }
    for a in config.accounts.iter_mut().filter(|a| a.id == id) {
        a.folder_id = folder.map(str::to_string);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;

    fn home(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wg-accounts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn add_rename_remove() {
        let h = home("arr");
        let mut c = default_config();
        // The OS's separator after `~`, as Node's path.join gives the Electron app.
        let tilde = |rest: &str| format!("~{}", rest.replace('/', std::path::MAIN_SEPARATOR_STR));
        let id = add_account(&mut c, "  Work Stuff ", "#7c5cff", AgentCli::Claude, &h).unwrap();
        assert_eq!(
            c.accounts[0].config_dir,
            tilde("/.wraithgrid/accounts/work-stuff")
        );
        assert!(h.join(".wraithgrid/accounts/work-stuff").is_dir());
        assert!(!c.accounts[0].signed_in);
        assert_eq!(c.settings.default_account_id.as_deref(), Some(id.as_str()));
        assert_eq!(
            add_account(&mut c, "work stuff", "#000000", AgentCli::Codex, &h).unwrap_err(),
            "An account with that name already exists."
        );
        let second = add_account(&mut c, "Work-Stuff!", "#2dd4bf", AgentCli::Gemini, &h).unwrap();
        assert_eq!(
            c.accounts[1].config_dir,
            tilde("/.wraithgrid/accounts/work-stuff-2")
        );
        assert!(c.accounts[1].signed_in);
        assert_eq!(
            rename_account(&mut c, &second, "Work Stuff").unwrap_err(),
            "An account with that name already exists."
        );
        rename_account(&mut c, &second, "Gem").unwrap();
        assert_eq!(c.accounts[1].name, "Gem");
        remove_account(&mut c, &id);
        assert_eq!(c.accounts.len(), 1);
        assert_eq!(c.settings.default_account_id, None);
        assert!(
            h.join(".wraithgrid/accounts/work-stuff").is_dir(),
            "the folder stays"
        );
        let _ = std::fs::remove_dir_all(h);
    }

    #[test]
    fn colors_and_slugs() {
        let mut c = default_config();
        let h = home("colors");
        assert_eq!(next_free_color(&c.accounts), "#7c5cff");
        add_account(&mut c, "a", "#7C5CFF", AgentCli::Claude, &h).unwrap();
        assert_eq!(next_free_color(&c.accounts), "#2dd4bf");
        let taken: HashSet<String> = ["x".to_string(), "x-2".to_string()].into();
        assert_eq!(unique_slug("x", &taken), "x-3");
        assert_eq!(unique_slug("", &HashSet::new()), "account");
        let _ = std::fs::remove_dir_all(h);
    }

    #[test]
    fn folders_group_accounts() {
        let dir = home("folders");
        let mut c = default_config();
        let a = add_account(&mut c, "work", "", AgentCli::Claude, &dir).unwrap();
        assert_eq!(create_folder(&mut c, "  "), None);
        let f = create_folder(&mut c, "Clients").unwrap();
        move_account_to_folder(&mut c, &a, Some(&f));
        assert_eq!(
            c.account(&a).unwrap().folder_id.as_deref(),
            Some(f.as_str())
        );
        move_account_to_folder(&mut c, &a, Some("f-missing"));
        assert_eq!(
            c.account(&a).unwrap().folder_id.as_deref(),
            Some(f.as_str())
        );
        rename_folder(&mut c, &f, "");
        rename_folder(&mut c, &f, "Team");
        toggle_folder(&mut c, &f);
        assert_eq!(c.account_folders[0].name, "Team");
        assert!(c.account_folders[0].collapsed);
        delete_folder(&mut c, &f);
        assert!(c.account_folders.is_empty());
        assert_eq!(c.account(&a).unwrap().folder_id, None);
        for i in 0..FOLDERS_MAX {
            assert!(create_folder(&mut c, &format!("F{i}")).is_some());
        }
        assert_eq!(create_folder(&mut c, "one too many"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn imports_a_config_dir_where_it_is() {
        let mut c = default_config();
        assert_eq!(import_account(&mut c, " "), Err("Choose a folder.".into()));
        let id = import_account(&mut c, "~/.claude").unwrap();
        let a = c.account(&id).unwrap();
        assert_eq!(
            (a.name.as_str(), a.imported, a.signed_in),
            ("claude", true, true)
        );
        assert_eq!(a.config_dir, "~/.claude");
        assert_eq!(c.settings.default_account_id.as_deref(), Some(id.as_str()));
        assert_eq!(
            import_account(&mut c, "~/.claude"),
            Err("That folder is already an account.".into())
        );
        let other = import_account(&mut c, "/x/claude").unwrap();
        assert_eq!(c.account(&other).unwrap().name, "claude-2");
    }

    #[test]
    fn deletes_only_an_accounts_dir_inside_home() {
        let h = home("deldir");
        let outside = home("deldir-outside");
        std::fs::create_dir_all(h.join("acc")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let mut c = default_config();
        import_account(&mut c, "~/acc").unwrap();
        import_account(&mut c, &outside.to_string_lossy()).unwrap();
        import_account(&mut c, "~").unwrap();
        // Not an account's dir.
        assert!(deletable_config_dir(&c, "~/other", &h).is_err());
        // Outside home, and home itself.
        assert!(deletable_config_dir(&c, &outside.to_string_lossy(), &h).is_err());
        assert!(deletable_config_dir(&c, "~", &h).is_err());
        // Inside home: its real path.
        let ok = deletable_config_dir(&c, "~/acc", &h).unwrap().unwrap();
        assert_eq!(ok, std::fs::canonicalize(h.join("acc")).unwrap());
        // A link inside home that points out of it.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, h.join("link")).unwrap();
            import_account(&mut c, "~/link").unwrap();
            assert!(deletable_config_dir(&c, "~/link", &h).is_err());
        }
        // Gone already.
        import_account(&mut c, "~/gone").unwrap();
        assert_eq!(deletable_config_dir(&c, "~/gone", &h), Ok(None));
        let _ = std::fs::remove_dir_all(&h);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
