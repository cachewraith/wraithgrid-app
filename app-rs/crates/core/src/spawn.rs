//! Turns "run this account's CLI (or a shell) in this folder" into the exact process to start:
//! binary, args, cwd and env (`src/main/spawn-resolve.ts`). The window does it for panes on
//! this computer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agent_cli::{with_project_doc_args, with_resume_args, AgentCli};
use crate::host::protocol::SpawnRequest;
use crate::pane_env::{build_pane_env, Os, PaneEnvInput};
use crate::paths::{expand_home, resolve_user_path};
use crate::platform::{find_on_path, resolve_pane_shell, spawn_command_for, Args};

/// What a pane runs, before anything machine-specific is looked up.
#[derive(Clone, Debug, PartialEq)]
pub struct SpawnSpec {
    pub pane_id: String,
    pub cli: AgentCli,
    pub shell: bool,
    /// The account's config dir, `~` meaning the home of the machine it runs on.
    pub config_dir: Option<String>,
    pub cwd: String,
    pub args: Vec<String>,
    pub resume: bool,
    pub cols: u16,
    pub rows: u16,
}

/// This machine's facts and the user's overrides that apply on it.
pub struct ResolveContext<'a> {
    pub os: Os,
    pub env: &'a BTreeMap<String, String>,
    pub home: &'a Path,
    /// claude binary override; empty: found on PATH.
    pub claude_path: &'a str,
    pub cli_paths: &'a [(AgentCli, String)],
    pub shell_path: &'a str,
    pub shell_args: &'a [String],
    pub mod_dirs: &'a [String],
}

/// An agent pane's arguments: the account's, plus resume and project-doc flags of its CLI.
pub fn agent_args(spec: &SpawnSpec) -> Vec<String> {
    let args = if spec.resume {
        with_resume_args(spec.cli, &spec.args)
    } else {
        spec.args.clone()
    };
    if spec.cli == AgentCli::Claude {
        args
    } else {
        with_project_doc_args(spec.cli, &args)
    }
}

pub fn resolve_spawn(spec: &SpawnSpec, ctx: &ResolveContext) -> Result<SpawnRequest, String> {
    let home = ctx.home.to_string_lossy().into_owned();
    let cwd = resolve_user_path(&spec.cwd, ctx.home);
    match std::fs::metadata(&cwd) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(format!("Not a folder: {}", spec.cwd)),
        Err(_) => return Err(format!("Folder not found: {}", spec.cwd)),
    }
    let env_get = |k: &str| ctx.env.get(k).cloned();
    let find = |name: &str| find_on_path(name, env_get, ctx.os);
    let (file, args): (PathBuf, Vec<String>) = if spec.shell {
        let exists = |p: &Path| p.exists();
        // A bare name stays bare: it is looked up on PATH.
        let picked = resolve_pane_shell(
            ctx.os,
            &env_get,
            &find,
            &exists,
            &expand_home(ctx.shell_path.trim(), &home),
            ctx.shell_args,
        )?;
        (picked.file, picked.args)
    } else if spec.cli != AgentCli::Claude {
        let bin = spec.cli.spec().bin;
        let override_path = ctx
            .cli_paths
            .iter()
            .find(|(c, _)| *c == spec.cli)
            .map(|(_, p)| expand_home(p.trim(), &home))
            .unwrap_or_default();
        let file = if !override_path.is_empty() {
            match std::fs::metadata(&override_path) {
                Ok(m) if m.is_file() => PathBuf::from(override_path),
                Ok(_) => return Err(format!("Not a file: {override_path}")),
                Err(_) => return Err(format!("{bin} binary not found: {override_path}")),
            }
        } else {
            find(bin).ok_or_else(|| {
                format!("{bin} ({}) was not found on PATH.", spec.cli.spec().label)
            })?
        };
        (file, agent_args(spec))
    } else {
        let file = if ctx.claude_path.trim().is_empty() {
            find("claude").ok_or("claude was not found on PATH. Set its path in Settings.")?
        } else {
            resolve_user_path(ctx.claude_path.trim(), ctx.home)
        };
        match std::fs::metadata(&file) {
            Ok(m) if m.is_file() => {}
            Ok(_) => return Err(format!("Not a file: {}", file.display())),
            Err(_) => return Err(format!("claude binary not found: {}", file.display())),
        }
        (file, agent_args(spec))
    };

    let (file, args) = spawn_command_for(
        ctx.os,
        &file.to_string_lossy(),
        &args,
        ctx.env.get("ComSpec").map(String::as_str),
    )?;
    let mut env = build_pane_env(PaneEnvInput {
        base: ctx.env,
        shell: spec.shell,
        cli: spec.cli,
        config_dir: spec.config_dir.as_deref(),
        home: ctx.home,
        mod_dirs: ctx.mod_dirs,
        os: ctx.os,
    });
    // Its own id, so an agent can name itself to `wraithgrid` (send, split, wait).
    env.insert("WRAITHGRID_PANE".into(), spec.pane_id.clone());
    Ok(SpawnRequest {
        pane_id: spec.pane_id.clone(),
        file,
        args: match args {
            Args::List(list) => Value::from(list),
            Args::Verbatim(line) => Value::from(line),
        },
        cwd: cwd.to_string_lossy().into_owned(),
        env,
        cols: spec.cols,
        rows: spec.rows,
        // Lets the host tell `wraithgrid wait --until blocked` the CLI is asking for approval.
        approval: (!spec.shell).then(|| spec.cli.spec().approval.to_string()),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn setup() -> (PathBuf, BTreeMap<String, String>) {
        let dir = std::env::temp_dir().join(format!("wg-spawn-{}", std::process::id()));
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(dir.join("proj")).unwrap();
        for name in ["claude", "codex", "fish"] {
            let f = bin.join(name);
            std::fs::write(&f, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let env = BTreeMap::from([
            ("PATH".to_string(), bin.to_string_lossy().into_owned()),
            (
                "SHELL".to_string(),
                bin.join("fish").to_string_lossy().into_owned(),
            ),
        ]);
        (dir, env)
    }

    fn spec(cli: AgentCli, shell: bool) -> SpawnSpec {
        SpawnSpec {
            pane_id: "p1".into(),
            cli,
            shell,
            config_dir: Some("~/.wraithgrid/accounts/a".into()),
            cwd: "~/proj".into(),
            args: vec![],
            resume: true,
            cols: 80,
            rows: 24,
        }
    }

    #[test]
    fn resolves_agents_and_shells() {
        let (home, env) = setup();
        let ctx = ResolveContext {
            os: Os::Linux,
            env: &env,
            home: &home,
            claude_path: "",
            cli_paths: &[],
            shell_path: "",
            shell_args: &[],
            mod_dirs: &[],
        };
        let claude = resolve_spawn(&spec(AgentCli::Claude, false), &ctx).unwrap();
        assert!(claude.file.ends_with("/bin/claude"));
        assert_eq!(claude.args, serde_json::json!(["--continue"]));
        assert_eq!(claude.env["WRAITHGRID_PANE"], "p1");
        assert!(claude.env["CLAUDE_CONFIG_DIR"].ends_with("/.wraithgrid/accounts/a"));
        assert!(claude.approval.is_some());
        let codex = resolve_spawn(&spec(AgentCli::Codex, false), &ctx).unwrap();
        assert_eq!(
            codex.args,
            serde_json::json!([
                "-c",
                "project_doc_fallback_filenames=[\"CLAUDE.md\"]",
                "resume",
                "--last"
            ])
        );
        let shell = resolve_spawn(&spec(AgentCli::Claude, true), &ctx).unwrap();
        assert!(shell.file.ends_with("/bin/fish"));
        assert!(shell.approval.is_none() && !shell.env.contains_key("CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn errors_read_like_the_ts() {
        let (home, env) = setup();
        let ctx = ResolveContext {
            os: Os::Linux,
            env: &env,
            home: &home,
            claude_path: "",
            cli_paths: &[],
            shell_path: "",
            shell_args: &[],
            mod_dirs: &[],
        };
        let mut s = spec(AgentCli::Gemini, false);
        assert_eq!(
            resolve_spawn(&s, &ctx).unwrap_err(),
            "gemini (Gemini CLI) was not found on PATH."
        );
        s.cwd = "~/nope".into();
        assert_eq!(
            resolve_spawn(&s, &ctx).unwrap_err(),
            "Folder not found: ~/nope"
        );
        let overrides = [(AgentCli::Gemini, "/missing/gemini".to_string())];
        let ctx2 = ResolveContext {
            cli_paths: &overrides,
            ..ctx
        };
        assert_eq!(
            resolve_spawn(&spec(AgentCli::Gemini, false), &ctx2).unwrap_err(),
            "gemini binary not found: /missing/gemini"
        );
    }
}
