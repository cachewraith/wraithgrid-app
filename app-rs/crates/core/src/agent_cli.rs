//! Everything Wraithgrid knows about each agent CLI, in one table (`src/shared/agent-clis.ts`),
//! so supporting a new CLI is one entry here instead of branches across the app.

use std::sync::OnceLock;

use regex::Regex;

use crate::types::str_enum;

str_enum!(
    /// The agent CLI an account's panes run.
    AgentCli {
        Claude = "claude",
        Gemini = "gemini",
        Agy = "agy",
        Codex = "codex",
        Opencode = "opencode",
        Cursor = "cursor",
        Copilot = "copilot",
    }
);

/// Where an account's sign-in lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignIn {
    /// In the config dir, so every account signs in on its own (checked for this CLI).
    Dir,
    /// The config dir moves the CLI's files, but its sign-in may be in the system keyring.
    DirUnchecked,
    /// No per-account dir at all; every account of this CLI uses the same login.
    Shared,
}

/// How a pasted image file is typed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImagePaste {
    Path,
    AtPath,
}

pub struct AgentCliSpec {
    pub label: &'static str,
    /// Short provider name for the account picker and cross-CLI handoffs.
    pub provider_label: &'static str,
    /// The command it is found by on PATH.
    pub bin: &'static str,
    /// The variable that points it at an account's config dir, or None when it has none.
    pub config_dir_var: Option<&'static str>,
    pub sign_in: SignIn,
    pub image_paste: ImagePaste,
    /// Bytes Shift+Enter sends for "new line", or None to leave the key alone.
    pub new_line: Option<&'static str>,
    /// Args that continue the folder's last conversation, or None when the CLI has none.
    pub resume_args: Option<&'static [&'static str]>,
    /// Output that means "waiting for the user to approve something" (case-insensitive).
    pub approval: &'static str,
    /// Has a chat-view adapter; without one its panes are terminals only.
    pub chat: bool,
    /// Its chats report plan limits; chats without them report tokens only.
    pub reports_limits: bool,
    /// Accounts screen hint shown when adding an account.
    pub note: &'static str,
}

// claude's prompts; the others are what their docs and screenshots show, until fixtures from
// real output replace them.
const CLAUDE_APPROVAL: &str =
    r"Do you want to (?:proceed|make this edit|create|allow|run)[^\n]{0,160}\?";
const GEMINI_APPROVAL: &str = r"(?:Allow execution|Apply this change|Do you want to proceed|Allow (?:once|always))[^\n]{0,160}\?";
const CODEX_APPROVAL: &str = r"(?:Would you like to (?:run|make|apply)|Allow command|Approve)[^\n]{0,160}\?|\[y/N\]|Yes, proceed";
const GENERIC_APPROVAL: &str = r"(?:Do you want to (?:proceed|run|allow|apply)|Allow (?:this|command|execution)|Run this command)[^\n]{0,160}\?|\[y/N\]";

static CLAUDE: AgentCliSpec = AgentCliSpec {
    provider_label: "Claude",
    label: "Claude Code",
    bin: "claude",
    config_dir_var: Some("CLAUDE_CONFIG_DIR"),
    sign_in: SignIn::Dir,
    image_paste: ImagePaste::Path,
    // claude's only "new line" key is ESC CR (what /terminal-setup configures elsewhere).
    new_line: Some("\x1b\r"),
    resume_args: Some(&["--continue"]),
    approval: CLAUDE_APPROVAL,
    chat: true,
    reports_limits: true,
    note: "After saving, use Login to open a pane running claude under this account and run /login there.",
};

static GEMINI: AgentCliSpec = AgentCliSpec {
    provider_label: "Gemini",
    label: "Gemini CLI",
    bin: "gemini",
    config_dir_var: Some("GEMINI_CLI_HOME"),
    sign_in: SignIn::Dir,
    image_paste: ImagePaste::AtPath,
    new_line: None,
    resume_args: Some(&["--resume", "latest"]),
    approval: GEMINI_APPROVAL,
    chat: false,
    reports_limits: false,
    note: "Panes run gemini with GEMINI_CLI_HOME set to this config dir; it asks you to sign in on first launch.",
};

static AGY: AgentCliSpec = AgentCliSpec {
    provider_label: "Antigravity",
    label: "Antigravity CLI",
    bin: "agy",
    config_dir_var: None,
    sign_in: SignIn::Shared,
    image_paste: ImagePaste::AtPath,
    new_line: None,
    resume_args: Some(&["--continue"]),
    approval: GEMINI_APPROVAL,
    chat: true,
    reports_limits: false,
    note: "Panes run agy. It keeps its sign-in in the system keyring, so every Antigravity account on this machine uses the same Google login; it asks on first launch.",
};

static CODEX: AgentCliSpec = AgentCliSpec {
    provider_label: "Codex",
    label: "Codex CLI",
    bin: "codex",
    config_dir_var: Some("CODEX_HOME"),
    sign_in: SignIn::DirUnchecked,
    image_paste: ImagePaste::Path,
    new_line: None,
    resume_args: Some(&["resume", "--last"]),
    approval: CODEX_APPROVAL,
    chat: true,
    reports_limits: true,
    note: "Panes run codex with CODEX_HOME set to this config dir. Its sign-in stays separate per account only while codex stores credentials in a file (cli_auth_credentials_store = \"file\"); with the keyring, accounts share one login.",
};

static OPENCODE: AgentCliSpec = AgentCliSpec {
    provider_label: "OpenCode",
    label: "opencode",
    bin: "opencode",
    // OPENCODE_CONFIG_DIR adds agents and commands but not auth.json, which lives in the
    // XDG data dir; moving that would move every tool the agent runs too.
    config_dir_var: None,
    sign_in: SignIn::Shared,
    image_paste: ImagePaste::AtPath,
    new_line: None,
    resume_args: Some(&["--continue"]),
    approval: GENERIC_APPROVAL,
    chat: false,
    reports_limits: false,
    note: "Panes run opencode. It keeps provider keys in one place for your user (~/.local/share/opencode), so every opencode account on this machine shares them.",
};

static CURSOR: AgentCliSpec = AgentCliSpec {
    provider_label: "Cursor",
    label: "Cursor Agent",
    bin: "cursor-agent",
    config_dir_var: Some("CURSOR_CONFIG_DIR"),
    sign_in: SignIn::DirUnchecked,
    image_paste: ImagePaste::AtPath,
    new_line: None,
    resume_args: Some(&["--continue"]),
    approval: GENERIC_APPROVAL,
    chat: false,
    reports_limits: false,
    note: "Panes run cursor-agent with CURSOR_CONFIG_DIR set to this config dir. Cursor does not document where it keeps the sign-in, so accounts may share one login; run cursor-agent login in a pane to check.",
};

static COPILOT: AgentCliSpec = AgentCliSpec {
    provider_label: "Copilot",
    label: "Copilot CLI",
    bin: "copilot",
    config_dir_var: Some("COPILOT_HOME"),
    sign_in: SignIn::DirUnchecked,
    image_paste: ImagePaste::AtPath,
    new_line: None,
    resume_args: Some(&["--continue"]),
    approval: GENERIC_APPROVAL,
    chat: false,
    reports_limits: false,
    note: "Panes run copilot with COPILOT_HOME set to this config dir. Copilot may keep its GitHub token in the system keychain, so accounts may share one login; use /login in a pane to check.",
};

impl AgentCli {
    pub fn spec(self) -> &'static AgentCliSpec {
        match self {
            AgentCli::Claude => &CLAUDE,
            AgentCli::Gemini => &GEMINI,
            AgentCli::Agy => &AGY,
            AgentCli::Codex => &CODEX,
            AgentCli::Opencode => &OPENCODE,
            AgentCli::Cursor => &CURSOR,
            AgentCli::Copilot => &COPILOT,
        }
    }

    /// The approval pattern, compiled once (case-insensitive like the TS `/i`).
    pub fn approval_regex(self) -> &'static Regex {
        static CACHE: OnceLock<Vec<Regex>> = OnceLock::new();
        let all = CACHE.get_or_init(|| {
            AgentCli::ALL
                .iter()
                .map(|c| Regex::new(&format!("(?i){}", c.spec().approval)).expect("approval regex"))
                .collect()
        });
        &all[AgentCli::ALL.iter().position(|c| *c == self).unwrap()]
    }

    /// CLIs whose Login opens the sign-in dialog; the rest sign in in a terminal.
    pub fn dialog_login(self) -> bool {
        matches!(self, AgentCli::Claude | AgentCli::Codex)
    }

    /// CLIs whose chats can carry on as another account of the same CLI (#29, #60).
    pub fn can_continue_as(self) -> bool {
        matches!(self, AgentCli::Claude | AgentCli::Codex)
    }
}

/// Every config-dir variable any CLI reads; an agent pane gets only its own one.
pub fn config_dir_vars() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for cli in AgentCli::ALL {
        if let Some(var) = cli.spec().config_dir_var {
            if !out.contains(&var) {
                out.push(var);
            }
        }
    }
    out
}

/// Prepends the CLI's resume args to a pane's args, unless the pane already resumes.
pub fn with_resume_args(cli: AgentCli, args: &[String]) -> Vec<String> {
    let Some(resume) = cli.spec().resume_args else {
        return args.to_vec();
    };
    // A pane started with its own resume/continue flag or subcommand is left as it is.
    let resumes = ["-c", "-r", "--continue", "--resume", "resume"];
    if args.iter().any(|a| resumes.contains(&a.as_str())) {
        return args.to_vec();
    }
    resume
        .iter()
        .map(|s| s.to_string())
        .chain(args.iter().cloned())
        .collect()
}

/// Codex falls back to the project's CLAUDE.md when there is no AGENTS.md
/// (`project_doc_fallback_filenames`). A global `-c` override, so it goes before any
/// subcommand and after `with_resume_args`, which reads a leading `-c` as claude's continue.
pub fn with_project_doc_args(cli: AgentCli, args: &[String]) -> Vec<String> {
    if cli != AgentCli::Codex {
        return args.to_vec();
    }
    ["-c", "project_doc_fallback_filenames=[\"CLAUDE.md\"]"]
        .iter()
        .map(|s| s.to_string())
        .chain(args.iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn resume_args() {
        assert_eq!(
            with_resume_args(AgentCli::Claude, &s(&["--model", "x"])),
            s(&["--continue", "--model", "x"])
        );
        assert_eq!(with_resume_args(AgentCli::Claude, &s(&["-r"])), s(&["-r"]));
        assert_eq!(
            with_resume_args(AgentCli::Codex, &[]),
            s(&["resume", "--last"])
        );
        assert_eq!(
            with_resume_args(AgentCli::Gemini, &[]),
            s(&["--resume", "latest"])
        );
    }

    #[test]
    fn project_doc_only_for_codex() {
        assert_eq!(
            with_project_doc_args(AgentCli::Claude, &s(&["a"])),
            s(&["a"])
        );
        assert_eq!(with_project_doc_args(AgentCli::Codex, &s(&["a"]))[0], "-c");
    }

    #[test]
    fn approvals() {
        assert!(AgentCli::Claude
            .approval_regex()
            .is_match("Do you want to proceed with this?"));
        assert!(AgentCli::Codex.approval_regex().is_match("continue? [y/N]"));
        assert!(!AgentCli::Claude.approval_regex().is_match("hello"));
    }

    #[test]
    fn config_dir_vars_are_unique() {
        let vars = config_dir_vars();
        assert!(vars.contains(&"CLAUDE_CONFIG_DIR") && vars.contains(&"CODEX_HOME"));
        assert_eq!(vars.len(), 5);
    }
}
