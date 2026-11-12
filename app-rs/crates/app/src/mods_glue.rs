//! What the mods crate needs from the app (`ipc.ts` mod and shared-config handlers): the
//! accounts, the claude binary and each account's pane environment, built from the config.

use std::path::PathBuf;

use wraithgrid_core::agent_cli::AgentCli;
use wraithgrid_core::config::Config;
use wraithgrid_core::pane_env::{build_pane_env, Os, PaneEnvInput};
use wraithgrid_core::paths::{home_dir, resolve_user_path};
use wraithgrid_core::platform::find_on_path;
use wraithgrid_mods::{Account, SharedBrains, SharedMode};

pub fn accounts(config: &Config) -> Vec<Account> {
    config
        .accounts
        .iter()
        .map(|a| Account {
            id: a.id.clone(),
            name: a.name.clone(),
            cli: a.cli.as_str().into(),
            config_dir: a.config_dir.clone(),
        })
        .collect()
}

pub fn shared_mode(config: &Config) -> SharedMode {
    match config.settings.shared_mode {
        wraithgrid_core::types::SharedMode::Overall => SharedMode::Overall,
        wraithgrid_core::types::SharedMode::PerAccount => SharedMode::PerAccount,
    }
}

fn log(msg: &str) {
    eprintln!("[wraithgrid] {msg}");
}

/// Links the provider's shared instructions and skills into an account before its pane
/// starts, when sharing is overall (`syncShared`). Problems are logged, never raised.
pub fn sync_before_start(config: &Config, account_id: &str) {
    if let Some(account) = accounts(config).into_iter().find(|a| a.id == account_id) {
        SharedBrains::new(home_dir()).sync_shared(shared_mode(config), &account, &log);
    }
}

/// Settings → Shared instructions and skills: links every account, or takes the links out.
pub fn apply_shared_mode(config: &Config) -> Result<usize, String> {
    SharedBrains::new(home_dir())
        .apply(shared_mode(config), &accounts(config), &log)
        .map(|r| r.len())
}

/// The claude binary: the Settings override, else the first on PATH.
pub fn claude_binary(
    config: &Config,
    env: &std::collections::BTreeMap<String, String>,
) -> Option<PathBuf> {
    if !config.claude_path.trim().is_empty() {
        return Some(resolve_user_path(config.claude_path.trim(), &home_dir()));
    }
    find_on_path("claude", |k| env.get(k).cloned(), Os::current())
}

/// The environment an account's claude runs with, as its panes get it.
pub fn env_for(
    config: &Config,
    base: &std::collections::BTreeMap<String, String>,
    account: &Account,
) -> wraithgrid_mods::mods::Env {
    let home = home_dir();
    build_pane_env(PaneEnvInput {
        base,
        shell: false,
        cli: AgentCli::parse(&account.cli).unwrap_or(AgentCli::Claude),
        config_dir: Some(&account.config_dir),
        home: &home,
        mod_dirs: &config.settings.mod_dirs,
        os: Os::current(),
    })
    .into_iter()
    .collect()
}
