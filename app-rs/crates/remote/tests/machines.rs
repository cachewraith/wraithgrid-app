//! Machines over ssh (port of `tests/unit/machines.test.ts`): ~/.ssh/config hosts, ssh
//! arguments, the `host-stdio` bridge behind a window's `RemoteHost`, and what a pane says
//! when its machine's link drops. ssh is a fake; the ends are in-memory pipes. The config
//! checks of that suite live with core's config parser; the pane router is the app's.

mod support;

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use support::{pipe_pair, read_frame, FakeHost, FakeSsh};
use wraithgrid_core::agent_cli::AgentCli;
use wraithgrid_core::config::Machine;
use wraithgrid_core::host::client::{HostEvent, MismatchChoice};
use wraithgrid_core::host::protocol::SpawnRequest;
use wraithgrid_core::spawn::SpawnSpec;
use wraithgrid_remote::*;

#[test]
fn lists_host_names_without_patterns_or_comments() {
    let text = [
        "# work machines",
        "Host build gpu-box  # two at once",
        "  HostName 10.0.0.5",
        "Host *.internal",
        "host lab",
        "Host !bad -oProxyCommand=x",
        "Match host foo",
    ]
    .join("\n");
    assert_eq!(ssh_config_hosts(&text), ["build", "gpu-box", "lab"]);
    assert_eq!(ssh_config_hosts("Host a\r\nHost a b\r\n"), ["a", "b"]);
}

#[test]
fn ends_ssh_options_before_the_host() {
    let args = ssh_args("build", None);
    let dashes = args.iter().position(|a| a == "--").unwrap();
    assert_eq!(args[dashes..], ["--", "build", "wraithgrid", "host-stdio"]);
    let bridge = ["x".to_string(), "stdio".to_string()];
    assert_eq!(
        ssh_args("build", Some(&bridge))[dashes + 1..],
        ["build", "x", "stdio"]
    );
    assert!(args.iter().any(|a| a == "BatchMode=yes"));
}

type Events = Arc<Mutex<Vec<HostEvent>>>;

fn machine() -> Machine {
    Machine {
        id: "m1".into(),
        name: "build".into(),
        ssh_host: "me@build.local".into(),
    }
}

fn remote(ssh: Arc<FakeSsh>, events: Events) -> RemoteHost {
    RemoteHost::new(RemoteHostDeps {
        machine: machine(),
        app_version: "2.0.0".into(),
        program: "ssh".into(),
        ssh,
        binary: Arc::new(|_| None),
        on_event: Arc::new(move |e| events.lock().unwrap().push(e)),
        on_mismatch: Box::new(|_, _| MismatchChoice::Keep),
        log: Arc::new(|_| {}),
    })
}

fn spec(pane_id: &str, cwd: &str) -> SpawnSpec {
    SpawnSpec {
        pane_id: pane_id.into(),
        cli: AgentCli::Claude,
        shell: false,
        config_dir: Some("~/.wraithgrid/accounts/work".into()),
        cwd: cwd.into(),
        args: vec!["--verbose".into()],
        resume: false,
        cols: 80,
        rows: 24,
    }
}

/// A machine without the Rust host: ssh runs `wraithgrid host-stdio`, i.e. `run_bridge` in
/// front of a host it starts on first use. Returns the window's host, that host, and how
/// often it was started.
fn linked() -> (RemoteHost, Arc<FakeHost>, Arc<AtomicUsize>, Arc<FakeSsh>) {
    let server = Arc::new(FakeHost {
        token: Some("remote-token".into()),
        ..FakeHost::default()
    });
    let started = Arc::new(AtomicUsize::new(0));
    let up = Arc::new(AtomicBool::new(false));
    let far = {
        let (server, started, up) = (server.clone(), started.clone(), up.clone());
        move |input, output| {
            let (server, started, up, up2) =
                (server.clone(), started.clone(), up.clone(), up.clone());
            let _ = run_bridge(BridgeDeps {
                input,
                output,
                connect_host: Box::new(move || {
                    if !up.load(Ordering::SeqCst) {
                        return Err(std::io::ErrorKind::NotFound.into());
                    }
                    let (gui, (r, w)) = pipe_pair();
                    let server = server.clone();
                    std::thread::spawn(move || server.serve(r, w));
                    Ok(gui)
                }),
                start_host: Box::new(move || {
                    started.fetch_add(1, Ordering::SeqCst);
                    up2.store(true, Ordering::SeqCst);
                    Ok(())
                }),
                read_token: Box::new(|| Ok("remote-token\n".into())),
                resolve: Box::new(|spec| {
                    if spec.cwd == "~/missing" {
                        return Err("Folder not found: ~/missing".into());
                    }
                    Ok(SpawnRequest {
                        pane_id: spec.pane_id.clone(),
                        file: format!("/remote/bin/{}", spec.cli),
                        args: json!(spec.args),
                        cwd: "/home/remote/proj".into(),
                        env: BTreeMap::new(),
                        cols: spec.cols,
                        rows: spec.rows,
                        approval: None,
                    })
                }),
                start_timeout: Duration::from_secs(5),
            });
        }
    };
    let ssh = Arc::new(FakeSsh::new("Darwin arm64\n", far));
    let gui = remote(ssh.clone(), Events::default());
    (gui, server, started, ssh)
}

#[test]
fn starts_the_remote_host_signs_in_for_the_gui_and_resolves_panes_there() {
    let (gui, server, started, ssh) = linked();
    assert_eq!(gui.spawn(&spec("p1", "~/proj")), Ok(7));
    assert_eq!(started.load(Ordering::SeqCst), 1);
    let hello = &server.frames_of("hello")[0];
    assert_eq!(
        (&hello["token"], &hello["role"]),
        (&json!("remote-token"), &json!("gui"))
    );
    let req = &server.frames_of("create")[0]["req"];
    assert_eq!(req["file"], "/remote/bin/claude");
    assert_eq!(req["cwd"], "/home/remote/proj");
    assert_eq!(
        gui.probe(),
        Ok(Probe {
            app_version: "2.0.0".into(),
            live: 1
        })
    );
    // No Rust host for Darwin: plain `wraithgrid host-stdio`.
    let opened = &ssh.opens.lock().unwrap()[0];
    assert_eq!(
        opened[opened.len() - 3..],
        ["me@build.local", "wraithgrid", "host-stdio"]
    );
}

#[test]
fn reports_a_pane_the_remote_machine_cannot_start() {
    let (gui, _, _, _) = linked();
    assert_eq!(
        gui.spawn(&spec("p1", "~/missing")),
        Err("Folder not found: ~/missing".into())
    );
    // The link still works after a refused pane.
    assert!(gui.spawn(&spec("p2", "~/proj")).is_ok());
}

#[test]
fn the_bridge_refuses_an_invalid_pane_request() {
    let server = Arc::new(FakeHost::default());
    let ((mut from_bridge, mut to_bridge), (input, output)) = pipe_pair();
    std::thread::spawn(move || {
        run_bridge(BridgeDeps {
            input,
            output,
            connect_host: Box::new(move || {
                let (gui, (r, w)) = pipe_pair();
                let server = server.clone();
                std::thread::spawn(move || server.serve(r, w));
                Ok(gui)
            }),
            start_host: Box::new(|| Err("unused".into())),
            read_token: Box::new(|| Ok("t".into())),
            resolve: Box::new(|_| Err("unused".into())),
            start_timeout: Duration::from_secs(1),
        })
    });
    let hello = r#"{"t":"hello","protocol":1,"appVersion":"2.0.0","token":"","role":"gui"}"#;
    let spawn = r#"{"t":"spawn","id":3,"spec":{"paneId":"bad id"}}"#;
    // One at a time: the bridge's own reply could otherwise overtake the host's welcome.
    writeln!(to_bridge, "{hello}").unwrap();
    assert_eq!(read_frame(&mut from_bridge)["t"], "welcome");
    writeln!(to_bridge, "{spawn}").unwrap();
    assert_eq!(
        read_frame(&mut from_bridge),
        json!({ "t": "reply", "id": 3, "reply": { "ok": false, "error": "Invalid pane request" } })
    );
}

#[test]
fn a_linux_machine_gets_a_plan_for_the_copied_rust_host() {
    let server = Arc::new(FakeHost::default());
    let far = {
        let server = server.clone();
        move |r, w| server.serve(r, w)
    };
    let ssh = Arc::new(FakeSsh::new(
        "Linux x86_64\n__WRAITHGRID_HOST_PRESENT__\n",
        far,
    ));
    let gui = remote(ssh.clone(), Events::default());
    assert_eq!(gui.spawn(&spec("p1", "~/proj")), Ok(7));
    let plan = &server.frames_of("plan")[0]["plan"];
    assert_eq!(plan["paneId"], "p1");
    assert_eq!(plan["candidates"], json!(["claude"]));
    assert_eq!(plan["env"]["set"]["WRAITHGRID_PANE"], "p1");
    assert!(server.frames_of("spawn").is_empty());
    let opened = ssh.opens.lock().unwrap()[0].clone();
    assert_eq!(
        opened[opened.len() - 3..],
        [".wraithgrid/bin/wraithgrid-host-2.0.0", "stdio", "2.0.0"]
    );
    // The machine is prepared once; a new link reuses the copy.
    gui.detach();
    assert!(gui.spawn(&spec("p2", "~/proj")).is_ok());
    assert_eq!(ssh.runs.lock().unwrap().len(), 1);
    assert_eq!(ssh.opens.lock().unwrap().len(), 2);
}

#[test]
fn says_why_a_machine_could_not_be_reached() {
    let unreachable = |preflight: &str| {
        let mut ssh = FakeSsh::new(preflight, |_, _| {});
        ssh.fail = Some(
            "debug1: x\nssh: connect to host build.local port 22: Connection refused\n".into(),
        );
        remote(Arc::new(ssh), Events::default())
    };
    let why = "ssh: connect to host build.local port 22: Connection refused";
    assert_eq!(
        unreachable("Darwin arm64\n").spawn(&spec("p1", "~")),
        Err(format!("Could not reach build (me@build.local): {why}. It needs ssh with a key or agent, and Wraithgrid installed there with wraithgrid on PATH."))
    );
    assert_eq!(
        unreachable("Linux x86_64\n__WRAITHGRID_HOST_PRESENT__\n").probe(),
        Err(format!(
            "Could not reach build (me@build.local): {why}. It needs ssh with a key or agent."
        ))
    );
}

#[test]
fn marks_panes_of_a_dropped_machine_link_as_disconnected() {
    let server = Arc::new(FakeHost {
        cut_on_resize: true,
        ..FakeHost::default()
    });
    let far = {
        let server = server.clone();
        move |r, w| server.serve(r, w)
    };
    let events = Events::default();
    let host = remote(
        Arc::new(FakeSsh::new("Darwin arm64\n", far)),
        events.clone(),
    );
    host.create(SpawnRequest {
        pane_id: "p1".into(),
        file: "f".into(),
        args: json!([]),
        cwd: "/".into(),
        env: BTreeMap::new(),
        cols: 80,
        rows: 24,
        approval: None,
    })
    .unwrap();
    // The fake host drops the link on this.
    host.resize("p1", 100, 30);
    let deadline = Instant::now() + Duration::from_secs(5);
    while events.lock().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        *events.lock().unwrap(),
        [HostEvent::Exit {
            pane_id: "p1".into(),
            exit_code: -1,
            signal: None,
            error: Some(lost_message(&machine())),
        }]
    );
    assert_eq!(
        lost_message(&machine()),
        "Disconnected from build. Restart to reconnect; the agent may still be running there."
    );
}
