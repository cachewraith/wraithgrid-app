//! The environment child processes (claude for mods, dev servers for the preview) start with:
//! this process's env with the usual bin dirs added to PATH (a launcher's PATH often lacks
//! ~/.local/bin and friends), and an account's claude env as its panes get it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use wraithgrid_core::agent_cli::AgentCli;
use wraithgrid_core::config::Config;
use wraithgrid_core::pane_env::{build_pane_env, Os, PaneEnvInput};
use wraithgrid_core::paths::{home_dir, resolve_user_path};
use wraithgrid_core::platform::{extra_bin_dirs, find_on_path, merge_path};

pub fn spawn_env() -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    let os = Os::current();
    let home = home_dir().to_string_lossy().into_owned();
    let extra =
        extra_bin_dirs(os, &home, |k| std::env::var(k).ok()).join(&os.path_delimiter().to_string());
    let key = if cfg!(windows) { "Path" } else { "PATH" };
    let inherited = std::env::var(key).ok();
    env.insert(
        key.into(),
        merge_path(os.path_delimiter(), &[inherited.as_deref(), Some(&extra)]),
    );
    env
}

/// The claude binary: the Settings override, else the first on PATH.
pub fn claude_binary(config: &Config, env: &BTreeMap<String, String>) -> Option<PathBuf> {
    if !config.claude_path.trim().is_empty() {
        return Some(resolve_user_path(config.claude_path.trim(), &home_dir()));
    }
    find_on_path("claude", |k| env.get(k).cloned(), Os::current())
}

pub fn accounts(config: &Config) -> Vec<wraithgrid_mods::Account> {
    config
        .accounts
        .iter()
        .map(|a| wraithgrid_mods::Account {
            id: a.id.clone(),
            name: a.name.clone(),
            cli: a.cli.as_str().into(),
            config_dir: a.config_dir.clone(),
        })
        .collect()
}

/// The environment an account's claude runs with, as its panes get it.
pub fn account_env(
    config: &Config,
    base: &BTreeMap<String, String>,
    account: &wraithgrid_mods::Account,
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
