//! Adding, renaming and removing accounts (`addAccount`, `renameAccount`, `removeAccount` in
//! the Electron store, `account:createDir` in main). New accounts get a config dir under
//! ~/.wraithgrid/accounts named after them, written as `~/…` like the Electron app writes it.

use std::collections::HashSet;
use std::path::Path;

use crate::agent_cli::AgentCli;
use crate::config::{Account, Config, Extra};
use crate::ids::new_id;
use crate::paths::{accounts_root, base_name, contract_home, slugify};
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
        let id = add_account(&mut c, "  Work Stuff ", "#7c5cff", AgentCli::Claude, &h).unwrap();
        assert_eq!(
            c.accounts[0].config_dir,
            "~/.wraithgrid/accounts/work-stuff"
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
            "~/.wraithgrid/accounts/work-stuff-2"
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
}
