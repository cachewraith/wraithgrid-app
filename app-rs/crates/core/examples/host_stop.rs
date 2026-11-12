//! Stops every pane and the host for a test user-data dir. Refuses without
//! WRAITHGRID_USER_DATA_DIR, so it never stops the host of the user's real app.
//! `WRAITHGRID_USER_DATA_DIR=/tmp/… cargo run -p wraithgrid-core --example host_stop`
use std::time::Duration;
use wraithgrid_core::host::client::*;
use wraithgrid_core::host::paths::current_host_paths;
use wraithgrid_core::host::protocol::ClientFrame;

fn main() {
    if std::env::var_os("WRAITHGRID_USER_DATA_DIR").is_none() {
        eprintln!("refusing: set WRAITHGRID_USER_DATA_DIR to the test copy's dir");
        std::process::exit(2);
    }
    let paths = current_host_paths(&wraithgrid_core::paths::current_user_data_dir());
    let token = std::fs::read_to_string(&paths.token).expect("token");
    let stream = connect_socket(&paths.socket).expect("no host running");
    let conn = HostConnection::open(
        stream,
        token.trim(),
        "rust-stop",
        "gui",
        Box::new(|_| {}),
        Box::new(|| {}),
        Duration::from_secs(3),
    )
    .expect("hello");
    let r = conn.request(|id| ClientFrame::Shutdown { id, exit: true });
    println!("shutdown host pid {}: {:?}", conn.welcome.pid, r.0);
}
