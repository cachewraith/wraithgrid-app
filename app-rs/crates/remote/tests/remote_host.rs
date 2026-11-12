//! The Rust host on an ssh machine (port of `tests/unit/remote-host.test.ts`): the plan the
//! window sends, which machines get the static binary, the preflight and upload commands,
//! run for real by `sh` (and `fish` when installed) for the sweep, and `prepare_remote`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use wraithgrid_core::agent_cli::AgentCli;
use wraithgrid_core::pane_env::{apply_env_plan, build_pane_env, pane_env_plan, Os, PaneEnvInput};
use wraithgrid_core::spawn::SpawnSpec;
use wraithgrid_remote::*;

fn spec() -> SpawnSpec {
    SpawnSpec {
        pane_id: "p-1".into(),
        cli: AgentCli::Claude,
        shell: false,
        config_dir: Some("~/.wraithgrid/accounts/work".into()),
        cwd: "~/proj".into(),
        args: vec!["--model".into(), "opus".into()],
        resume: true,
        cols: 100,
        rows: 30,
    }
}

#[test]
fn the_plan_carries_the_rules_and_the_machine_fills_in_its_own_paths() {
    let plan = remote_plan(&spec());
    assert_eq!(plan.candidates, ["claude"]);
    assert!(plan.args.iter().any(|a| a == "--continue"));
    assert_eq!(plan.cwd, "~/proj");
    for (k, v) in [
        ("CLAUDE_CONFIG_DIR", "~/.wraithgrid/accounts/work"),
        ("WRAITHGRID_PANE", "p-1"),
        ("TERM", "xterm-256color"),
    ] {
        assert_eq!(plan.env.set.get(k).map(String::as_str), Some(v));
    }
    assert_eq!(plan.env.lang.as_deref(), Some("C.UTF-8"));
    assert!(plan.approval.is_some_and(|a| !a.is_empty()));
    assert!(!plan.path.by_kind["bash"].is_empty());
}

#[test]
fn a_shell_pane_tries_shell_first_and_keeps_the_account_dir_vars_out() {
    let plan = remote_plan(&SpawnSpec {
        shell: true,
        ..spec()
    });
    assert_eq!(plan.candidates[0], "$SHELL");
    assert!(plan.args.is_empty());
    assert!(!plan.env.drop.iter().any(|v| v == "CLAUDE_CONFIG_DIR"));
    assert!(!plan.env.set.contains_key("CLAUDE_CONFIG_DIR"));
    assert!(plan.approval.is_none());
}

#[test]
fn applied_locally_the_plan_builds_the_same_env_as_a_local_pane() {
    let base: BTreeMap<String, String> = [
        ("PATH", "/usr/bin"),
        ("TMUX", "x"),
        ("ELECTRON_RUN_AS_NODE", "1"),
        ("KEEP", "1"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let home = Path::new("/h");
    let local = build_pane_env(PaneEnvInput {
        base: &base,
        shell: false,
        cli: AgentCli::Claude,
        config_dir: None,
        home,
        mod_dirs: &[],
        os: Os::Linux,
    });
    let plan = pane_env_plan(false, AgentCli::Claude, None, Os::Linux);
    assert_eq!(apply_env_plan(&base, &plan, home), local);
}

#[test]
fn runs_on_linux_x64_and_arm64_only() {
    assert_eq!(
        remote_target("Linux x86_64\n"),
        Some("x86_64-unknown-linux-musl")
    );
    assert_eq!(
        remote_target("Linux aarch64"),
        Some("aarch64-unknown-linux-musl")
    );
    assert_eq!(remote_target("Darwin arm64"), None);
    assert_eq!(remote_target("Linux riscv64"), None);
    assert_eq!(remote_target(""), None);
}

#[test]
fn checks_that_the_binary_runs_before_trusting_it() {
    assert!(preflight_command("1.2.3").contains(&format!("{} --version", remote_bin_path("1.2.3"))));
    assert!(upload_command("1.2.3").contains("--version >/dev/null && mv -f"));
    // A fish function named find must not stand in for it (docs/CONTEXT.md).
    assert!(preflight_command("1.2.3").contains("command find .wraithgrid/bin"));
}

/// Run for real by the shells a machine may have: sh, and fish when it is installed here.
#[cfg(unix)]
#[test]
fn marks_the_version_in_use_and_deletes_others_unused_for_30_days() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::time::{Duration, SystemTime};

    let installed = |sh: &str| {
        Command::new("sh")
            .args(["-c", &format!("command -v {sh}")])
            .output()
            .is_ok_and(|o| o.status.success())
    };
    let day = Duration::from_secs(86_400);
    for sh in ["sh", "fish"].into_iter().filter(|sh| installed(sh)) {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".wraithgrid/bin");
        std::fs::create_dir_all(&dir).unwrap();
        let file = |version: &str, age_days: u32| {
            let name = Path::new(&remote_bin_path(version))
                .file_name()
                .unwrap()
                .to_owned();
            let f = dir.join(name);
            std::fs::write(&f, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o700)).unwrap();
            let t = SystemTime::now() - day * age_days;
            let handle = std::fs::File::options().write(true).open(&f).unwrap();
            handle.set_modified(t).unwrap();
            f
        };
        let current = file("2.0.0", 90);
        let stale = file("1.0.0", 40);
        let recent = file("1.9.0", 5);
        let out = Command::new(sh)
            .args(["-c", &preflight_command("2.0.0")])
            .current_dir(home.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("__WRAITHGRID_HOST_PRESENT__"),
            "{sh}: {stdout}"
        );
        assert!(!stale.exists(), "{sh}");
        assert!(recent.exists(), "{sh}");
        let touched = std::fs::metadata(&current).unwrap().modified().unwrap();
        assert!(
            touched.elapsed().unwrap_or_default() < Duration::from_secs(60),
            "{sh}"
        );
    }
}

type Runs = RefCell<Vec<(String, Option<Vec<u8>>)>>;

/// `prepare_remote` with a fake machine: `stdout` from the preflight, `upload_code` for the
/// copy, and `binary` as this build's binary for every target.
fn prepare(
    version: &str,
    stdout: &str,
    binary: Option<&[u8]>,
    upload_code: i32,
    runs: &Runs,
) -> Option<Vec<String>> {
    let run = |command: &str, input: Option<&[u8]>| {
        runs.borrow_mut()
            .push((command.to_string(), input.map(<[u8]>::to_vec)));
        Ok(match input {
            Some(_) => SshRun {
                code: Some(upload_code),
                ..SshRun::default()
            },
            None => SshRun {
                code: Some(0),
                stdout: stdout.into(),
                stderr: String::new(),
            },
        })
    };
    let binary = |_: &str| binary.map(<[u8]>::to_vec);
    prepare_remote(&PrepareDeps {
        version,
        run: &run,
        binary: &binary,
        log: &|_| {},
    })
    .unwrap()
}

fn command() -> Option<Vec<String>> {
    Some(vec![
        remote_bin_path("1.2.3"),
        "stdio".into(),
        "1.2.3".into(),
    ])
}

#[test]
fn uses_the_copy_already_there() {
    let runs = Runs::default();
    let out = prepare(
        "1.2.3",
        "Linux x86_64\n__WRAITHGRID_HOST_PRESENT__\n",
        None,
        0,
        &runs,
    );
    assert_eq!(out, command());
    assert_eq!(runs.borrow().len(), 1);
}

#[test]
fn copies_this_builds_binary_when_the_machine_has_none() {
    let runs = Runs::default();
    assert_eq!(
        prepare("1.2.3", "Linux aarch64\n", Some(b"elf"), 0, &runs),
        command()
    );
    assert_eq!(
        runs.borrow()[1],
        (upload_command("1.2.3"), Some(b"elf".to_vec()))
    );
}

#[test]
fn falls_back_to_wraithgrid_host_stdio_otherwise() {
    let runs = Runs::default();
    assert_eq!(
        prepare("1.2.3", "Darwin arm64\n", Some(b"x"), 0, &runs),
        None
    );
    assert_eq!(prepare("1.2.3", "Linux x86_64\n", None, 0, &runs), None);
    assert_eq!(
        prepare("1.2.3", "Linux x86_64\n", Some(b"x"), 1, &runs),
        None
    );
    let before = runs.borrow().len();
    assert_eq!(prepare("1; rm -rf ~", "", None, 0, &runs), None);
    // An unsafe version never reaches a shell.
    assert_eq!(runs.borrow().len(), before);
}

#[test]
fn a_failed_ssh_is_an_error_not_a_fallback() {
    let run = |_: &str, _: Option<&[u8]>| -> std::io::Result<SshRun> {
        Err(std::io::ErrorKind::NotFound.into())
    };
    let logged = RefCell::new(Vec::<String>::new());
    let log = |m: &str| logged.borrow_mut().push(m.into());
    let r = prepare_remote(&PrepareDeps {
        version: "1.2.3",
        run: &run,
        binary: &|_| None,
        log: &log,
    });
    assert!(r.is_err());
    let r = prepare_remote(&PrepareDeps {
        version: "1.2.3",
        run: &|_, _| {
            Ok(SshRun {
                code: Some(0),
                stdout: " Darwin arm64 \n".into(),
                stderr: String::new(),
            })
        },
        binary: &|_| None,
        log: &log,
    });
    assert_eq!(r, Ok(None));
    assert_eq!(
        logged.borrow()[0],
        "no Rust host for \"Darwin arm64\"; using wraithgrid host-stdio"
    );
}
