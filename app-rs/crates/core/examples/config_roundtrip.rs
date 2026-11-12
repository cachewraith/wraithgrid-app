//! Reads a config.json, validates it like the app does, and reports whether writing it back
//! gives the same bytes. `cargo run -p wraithgrid-core --example config_roundtrip -- FILE`
fn main() {
    let file = std::env::args().nth(1).expect("config file");
    let text = std::fs::read_to_string(&file).expect("read");
    let raw: serde_json::Value = serde_json::from_str(&text).expect("json");
    match wraithgrid_core::config::parse_config(&raw, true) {
        Err(err) => println!("REJECTED: {err}"),
        Ok(cfg) => {
            let out = cfg.to_json();
            println!(
                "ok: {} accounts, {} workspaces, {} panes; identical bytes: {}",
                cfg.accounts.len(),
                cfg.workspaces.len(),
                cfg.workspaces.iter().map(|w| w.panes.len()).sum::<usize>(),
                out == text
            );
            if out != text {
                std::fs::write("/tmp/wgspike/roundtrip.json", &out).unwrap();
            }
        }
    }
}
