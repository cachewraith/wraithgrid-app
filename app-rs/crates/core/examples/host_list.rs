//! Connects to the running host for the real user-data dir and lists its panes (read-only).
//! `cargo run -p wraithgrid-core --example host_list`
use std::sync::Arc;
use std::time::Duration;
use wraithgrid_core::host::client::*;
use wraithgrid_core::host::paths::current_host_paths;
use wraithgrid_core::host::protocol::ClientFrame;

fn main() {
    let user_data = wraithgrid_core::paths::current_user_data_dir();
    let paths = current_host_paths(&user_data);
    println!(
        "user data: {}\nsocket: {} (exists: {})",
        user_data.display(),
        paths.socket.display(),
        paths.socket.exists()
    );
    let token = std::fs::read_to_string(&paths.token).expect("token");
    let conn = HostConnection::open(
        connect_socket(&paths.socket).expect("connect"),
        token.trim(),
        "rust-probe",
        "gui",
        Box::new(|_| {}),
        Box::new(|| {}),
        Duration::from_secs(3),
    )
    .expect("hello");
    println!("welcome: {:?}", conn.welcome);
    let conn = Arc::new(conn);
    let r = conn.request(|id| ClientFrame::List { id });
    for p in r.0["panes"].as_array().into_iter().flatten() {
        println!(
            "  pane {} pid {} state {}",
            p["paneId"], p["pid"], p["state"]
        );
    }
    conn.close();
}
