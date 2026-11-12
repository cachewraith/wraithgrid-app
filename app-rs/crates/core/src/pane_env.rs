//! The one place that decides a pane's environment (`src/main/pane-env.ts`, requirements
//! §13), including the CLI's config-dir variable. The same rules run on a remote machine's
//! host, so they are data (`PaneEnvPlan`) applied to an environment.

use std::collections::BTreeMap;
use std::path::Path;

use crate::agent_cli::{config_dir_vars, AgentCli};
use crate::paths::{expand_home, resolve_user_path};

/// Variables that describe the terminal Wraithgrid was started from, not the pane. Inherited,
/// they make claude size itself wrong (COLUMNS/LINES) or act as if it ran inside VS Code,
/// tmux, kitty or Windows Terminal (keybindings, IDE hooks, graphics), breaking its screen.
pub const TERMINAL_VARS: &[&str] = &[
    "COLUMNS",
    "LINES",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_SESSION_ID",
    "TERMINAL_EMULATOR",
    "TMUX",
    "TMUX_PANE",
    "STY",
    "WINDOW",
    "VTE_VERSION",
    "WT_SESSION",
    "WT_PROFILE_ID",
    "ALACRITTY_LOG",
    "ALACRITTY_SOCKET",
    "ALACRITTY_WINDOW_ID",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_BIN_DIR",
    "GHOSTTY_SHELL_FEATURES",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    // Set inside a claude session: a claude pane is never nested in one.
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
];
pub const TERMINAL_PREFIXES: &[&str] = &[
    "KITTY_",
    "WEZTERM_",
    "ITERM_",
    "KONSOLE_",
    "VSCODE_",
    "GNOME_TERMINAL_",
];

/// The variable claude reads extra plugin folders from, a path list like PATH.
pub const PLUGIN_DIRS_VAR: &str = "CLAUDE_CODE_PLUGIN_DIRS";

/// True for a variable inherited from the launching terminal that a pane must not see.
pub fn is_terminal_var(key: &str) -> bool {
    let k = key.to_uppercase();
    TERMINAL_VARS.contains(&k.as_str()) || TERMINAL_PREFIXES.iter().any(|p| k.starts_with(p))
}

/// A pane's environment as rules: what to drop, what to set and the locale to add. `set`
/// values starting with `~/` mean the home of the machine applying it.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneEnvPlan {
    /// Exact names (upper case) to leave out.
    pub drop: Vec<String>,
    /// Name prefixes (upper case) to leave out.
    pub drop_prefixes: Vec<String>,
    pub set: BTreeMap<String, String>,
    /// Set as LANG when none of LC_ALL, LC_CTYPE, LANG is; None on Windows.
    pub lang: Option<String>,
}

/// `process.platform` names, so the rules read like the TS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Mac,
    Windows,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            Os::Linux
        }
    }

    pub fn path_delimiter(self) -> char {
        if self == Os::Windows {
            ';'
        } else {
            ':'
        }
    }
}

pub fn pane_env_plan(shell: bool, cli: AgentCli, config_dir: Option<&str>, os: Os) -> PaneEnvPlan {
    let mut set = BTreeMap::new();
    // What the terminal is: every shell and claude read these.
    set.insert("TERM".into(), "xterm-256color".into());
    set.insert("COLORTERM".into(), "truecolor".into());
    if let (false, Some(dir), Some(var)) = (
        shell,
        config_dir.filter(|d| !d.is_empty()),
        cli.spec().config_dir_var,
    ) {
        set.insert(var.into(), dir.into());
    }
    let mut drop: Vec<String> = TERMINAL_VARS.iter().map(|s| s.to_string()).collect();
    // An agent pane sees only its own account's config dir, never one Wraithgrid inherited.
    if !shell {
        drop.extend(config_dir_vars().into_iter().map(String::from));
    }
    // Electron's own switches must not leak into child tools (ELECTRON_RUN_AS_NODE).
    let mut drop_prefixes = vec!["ELECTRON_".to_string()];
    drop_prefixes.extend(TERMINAL_PREFIXES.iter().map(|s| s.to_string()));
    PaneEnvPlan {
        drop,
        drop_prefixes,
        set,
        // macOS apps opened from the Dock get no locale; older macOS has no C.UTF-8.
        lang: match os {
            Os::Windows => None,
            Os::Mac => Some("en_US.UTF-8".into()),
            Os::Linux => Some("C.UTF-8".into()),
        },
    }
}

/// Applies a plan to an environment; `~` in a set value is `home`, made absolute.
pub fn apply_env_plan(
    base: &BTreeMap<String, String>,
    plan: &PaneEnvPlan,
    home: &Path,
) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = base
        .iter()
        .filter(|(key, _)| {
            let k = key.to_uppercase();
            !plan.drop.contains(&k) && !plan.drop_prefixes.iter().any(|p| k.starts_with(p.as_str()))
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (key, value) in &plan.set {
        let v = if value == "~" || value.starts_with("~/") {
            resolve_user_path(value, home)
                .to_string_lossy()
                .into_owned()
        } else {
            value.clone()
        };
        env.insert(key.clone(), v);
    }
    let has_locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .any(|k| env.get(*k).is_some_and(|v| !v.is_empty()));
    if let (Some(lang), false) = (&plan.lang, has_locale) {
        env.insert("LANG".into(), lang.clone());
    }
    env
}

/// The path list for PLUGIN_DIRS_VAR: what the user's environment already named, then the
/// mod folders from Settings as absolute paths, without repeats. A folder whose path holds
/// the list separator cannot be named in a path list and is left out.
pub fn plugin_dirs_value(
    inherited: Option<&str>,
    mod_dirs: &[String],
    home: &Path,
    os: Os,
) -> String {
    let sep = os.path_delimiter();
    let own = mod_dirs
        .iter()
        .map(|d| d.trim())
        .filter(|d| !d.is_empty())
        .map(|d| resolve_user_path(d, home).to_string_lossy().into_owned())
        .filter(|d| !d.contains(sep));
    let mut out: Vec<String> = Vec::new();
    for d in inherited
        .unwrap_or("")
        .split(sep)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .chain(own)
    {
        if !out.contains(&d) {
            out.push(d);
        }
    }
    out.join(&sep.to_string())
}

pub struct PaneEnvInput<'a> {
    pub base: &'a BTreeMap<String, String>,
    /// Plain shell panes never get a config dir from Wraithgrid.
    pub shell: bool,
    pub cli: AgentCli,
    /// The account's config dir, possibly starting with `~`.
    pub config_dir: Option<&'a str>,
    pub home: &'a Path,
    /// Mod folders from Settings; claude panes get them as CLAUDE_CODE_PLUGIN_DIRS.
    pub mod_dirs: &'a [String],
    pub os: Os,
}

/// The one place that decides a pane's environment.
pub fn build_pane_env(i: PaneEnvInput) -> BTreeMap<String, String> {
    let mut plan = pane_env_plan(i.shell, i.cli, i.config_dir, i.os);
    // A config dir that isn't under `~` is still made absolute here.
    if let Some(var) = i.cli.spec().config_dir_var {
        if let Some(dir) = plan.set.get(var).cloned() {
            let abs = resolve_user_path(&expand_home(&dir, &i.home.to_string_lossy()), i.home);
            plan.set
                .insert(var.into(), abs.to_string_lossy().into_owned());
        }
    }
    let mut env = apply_env_plan(i.base, &plan, i.home);
    if !i.shell && i.cli == AgentCli::Claude {
        let dirs = plugin_dirs_value(
            env.get(PLUGIN_DIRS_VAR).map(String::as_str),
            i.mod_dirs,
            i.home,
            i.os,
        );
        if !dirs.is_empty() {
            env.insert(PLUGIN_DIRS_VAR.into(), dirs);
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn env_for(
        shell: bool,
        cli: AgentCli,
        dir: Option<&str>,
        b: &BTreeMap<String, String>,
    ) -> BTreeMap<String, String> {
        build_pane_env(PaneEnvInput {
            base: b,
            shell,
            cli,
            config_dir: dir,
            home: Path::new("/home/u"),
            mod_dirs: &[],
            os: Os::Linux,
        })
    }

    #[test]
    fn terminal_vars_dropped() {
        let b = base(&[
            ("TMUX", "1"),
            ("KITTY_WINDOW_ID", "3"),
            ("ELECTRON_RUN_AS_NODE", "1"),
            ("PATH", "/bin"),
            ("columns", "80"),
        ]);
        let env = env_for(true, AgentCli::Claude, None, &b);
        assert_eq!(env.get("PATH").unwrap(), "/bin");
        assert!(
            !env.contains_key("TMUX")
                && !env.contains_key("KITTY_WINDOW_ID")
                && !env.contains_key("ELECTRON_RUN_AS_NODE")
                && !env.contains_key("columns")
        );
        assert_eq!(env.get("TERM").unwrap(), "xterm-256color");
        assert_eq!(env.get("LANG").unwrap(), "C.UTF-8");
    }

    #[test]
    fn agent_gets_only_its_own_config_dir() {
        let b = base(&[
            ("CODEX_HOME", "/elsewhere"),
            ("CLAUDE_CONFIG_DIR", "/inherited"),
            ("LANG", "de_DE.UTF-8"),
        ]);
        let env = env_for(
            false,
            AgentCli::Claude,
            Some("~/.wraithgrid/accounts/a"),
            &b,
        );
        assert_eq!(
            env.get("CLAUDE_CONFIG_DIR").unwrap(),
            "/home/u/.wraithgrid/accounts/a"
        );
        assert!(!env.contains_key("CODEX_HOME"));
        assert_eq!(env.get("LANG").unwrap(), "de_DE.UTF-8");
        // A shell pane keeps what it inherited and gets no account dir.
        let env = env_for(true, AgentCli::Claude, Some("/x"), &b);
        assert_eq!(env.get("CLAUDE_CONFIG_DIR").unwrap(), "/inherited");
    }

    #[test]
    fn cli_without_dir_var() {
        let env = env_for(false, AgentCli::Agy, Some("/x"), &base(&[]));
        assert!(!env.values().any(|v| v == "/x"));
    }

    #[test]
    fn plugin_dirs() {
        let home = Path::new("/home/u");
        let dirs = vec![
            "~/mods/a".to_string(),
            " ".to_string(),
            "/x:y".to_string(),
            "/opt/b".to_string(),
        ];
        assert_eq!(
            plugin_dirs_value(Some("/opt/b:/c"), &dirs, home, Os::Linux),
            "/opt/b:/c:/home/u/mods/a"
        );
        let b = base(&[]);
        let env = build_pane_env(PaneEnvInput {
            base: &b,
            shell: false,
            cli: AgentCli::Claude,
            config_dir: None,
            home,
            mod_dirs: &dirs,
            os: Os::Linux,
        });
        assert_eq!(env.get(PLUGIN_DIRS_VAR).unwrap(), "/home/u/mods/a:/opt/b");
    }
}
