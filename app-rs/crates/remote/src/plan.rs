//! A remote pane as a plan (ADR 0004 phase 5, `src/main/remote-plan.ts`): the window keeps
//! every rule (which CLI, its arguments, the environment, how to read the login shell's PATH)
//! and the Rust host on the ssh machine (`host-rs/src/plan.rs`) only does what needs that
//! machine: PATH from its login shell, the first candidate binary that exists, the folder,
//! `~` and its own environment.

use std::collections::BTreeMap;

use serde::{Serialize, Serializer};
use serde_json::json;
use wraithgrid_core::pane_env::{pane_env_plan, Os, PaneEnvPlan};
use wraithgrid_core::platform::{extra_bin_dirs, login_probe_args, ENV_MARK};
use wraithgrid_core::spawn::{agent_args, SpawnSpec};

/// The default shells to try on a Linux machine after its $SHELL (platform.ts).
const LINUX_SHELLS: &[&str] = &["/bin/bash", "/usr/bin/bash", "/bin/zsh", "/bin/sh"];

/// Shell kinds the machine's $SHELL may be, each with its own way to run the probe.
const PROBE_KINDS: &[&str] = &[
    "bash", "zsh", "sh", "dash", "ash", "ksh", "mksh", "oksh", "yash", "fish", "xonsh", "nu",
    "pwsh",
];

/// `/bin/sh -l -c <probe>`, asked when $SHELL gives nothing.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FallbackProbe {
    pub file: String,
    pub args: Vec<String>,
}

/// How the machine reads its login shell's PATH (`PathProbePlan` in platform.ts).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathProbePlan {
    /// The output between two of these is the PATH.
    pub mark: String,
    /// Arguments for $SHELL by its kind (`shell_kind`).
    pub by_kind: BTreeMap<String, Vec<String>>,
    /// Arguments for a shell of any other kind.
    pub otherwise: Vec<String>,
    pub fallback: FallbackProbe,
    /// Added after the probed PATH and the inherited one, `~` for the machine's home.
    pub extra_dirs: Vec<String>,
}

pub fn path_probe_plan() -> PathProbePlan {
    let otherwise = login_probe_args("/bin/unknown-shell");
    // Any other kind runs `-c <probe>`: the probe itself is its last argument.
    let probe = otherwise.last().cloned().unwrap_or_default();
    PathProbePlan {
        mark: ENV_MARK.into(),
        by_kind: PROBE_KINDS
            .iter()
            .map(|k| (k.to_string(), login_probe_args(&format!("/bin/{k}"))))
            .collect(),
        otherwise,
        fallback: FallbackProbe {
            file: "/bin/sh".into(),
            args: vec!["-l".into(), "-c".into(), probe],
        },
        extra_dirs: extra_bin_dirs(Os::Linux, "~", |_| None),
    }
}

/// What the `plan` frame carries (`RemotePlan` in remote-plan.ts).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePlan {
    pub pane_id: String,
    pub cols: u16,
    pub rows: u16,
    /// The folder, `~` for the machine's home; relative paths are under it.
    pub cwd: String,
    /// In order, the first that exists runs: `$SHELL` (the machine's login shell), a path
    /// (absolute or `~/…`) or a bare name looked up on the machine's PATH.
    pub candidates: Vec<String>,
    pub not_found: String,
    pub args: Vec<String>,
    #[serde(serialize_with = "env_plan")]
    pub env: PaneEnvPlan,
    pub path: PathProbePlan,
    /// The CLI's "needs approval" output as a RegExp source, for `blocked`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
}

/// Core's env plan in the TS field names.
fn env_plan<S: Serializer>(p: &PaneEnvPlan, s: S) -> Result<S::Ok, S::Error> {
    json!({
        "drop": p.drop,
        "dropPrefixes": p.drop_prefixes,
        "set": p.set,
        "lang": p.lang,
    })
    .serialize(s)
}

pub fn remote_plan(spec: &SpawnSpec) -> RemotePlan {
    let mut env = pane_env_plan(spec.shell, spec.cli, spec.config_dir.as_deref(), Os::Linux);
    // Its own id, so an agent can name itself to `wraithgrid` (send, split, wait).
    env.set
        .insert("WRAITHGRID_PANE".into(), spec.pane_id.clone());
    let cli = spec.cli.spec();
    RemotePlan {
        pane_id: spec.pane_id.clone(),
        cols: spec.cols,
        rows: spec.rows,
        cwd: spec.cwd.clone(),
        candidates: if spec.shell {
            std::iter::once("$SHELL")
                .chain(LINUX_SHELLS.iter().copied())
                .map(String::from)
                .collect()
        } else {
            vec![cli.bin.into()]
        },
        not_found: if spec.shell {
            "No shell found on that machine.".into()
        } else {
            format!("{} ({}) was not found on PATH there.", cli.bin, cli.label)
        },
        args: if spec.shell { vec![] } else { agent_args(spec) },
        env,
        path: path_probe_plan(),
        approval: (!spec.shell).then(|| cli.approval.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wraithgrid_core::agent_cli::AgentCli;

    #[test]
    fn serializes_in_the_ts_field_names() {
        let spec = SpawnSpec {
            pane_id: "p".into(),
            cli: AgentCli::Gemini,
            shell: false,
            config_dir: None,
            cwd: "~".into(),
            args: vec![],
            resume: false,
            cols: 80,
            rows: 24,
        };
        let v = serde_json::to_value(remote_plan(&spec)).unwrap();
        assert_eq!(v["paneId"], "p");
        assert_eq!(
            v["notFound"],
            "gemini (Gemini CLI) was not found on PATH there."
        );
        assert_eq!(v["env"]["lang"], "C.UTF-8");
        assert!(v["env"]["dropPrefixes"]
            .as_array()
            .unwrap()
            .contains(&json!("ELECTRON_")));
        assert_eq!(v["path"]["mark"], ENV_MARK);
        assert_eq!(v["path"]["fallback"]["args"][1], "-c");
        assert!(v["path"]["fallback"]["args"][2]
            .as_str()
            .unwrap()
            .contains("printenv PATH"));
        assert_eq!(v["path"]["extraDirs"][0], "~/.local/bin");
        assert!(v["approval"].is_string());
        let shell = serde_json::to_value(remote_plan(&SpawnSpec {
            shell: true,
            ..spec
        }))
        .unwrap();
        assert!(shell.get("approval").is_none());
        assert_eq!(shell["notFound"], "No shell found on that machine.");
        assert_eq!(shell["candidates"][4], "/bin/sh");
    }
}
