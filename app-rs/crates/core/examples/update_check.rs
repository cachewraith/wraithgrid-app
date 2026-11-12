//! Asks GitHub for the latest release, as the app's update check does (read only).
fn main() {
    let current = std::env::args().nth(1).unwrap_or_else(|| "1.0.0".into());
    let r = wraithgrid_core::update::check(&current, &|url| {
        wraithgrid_core::update::fetch(url, "Wraithgrid/dev")
    });
    match r {
        wraithgrid_core::update::CheckResult::Available { latest, .. }
        | wraithgrid_core::update::CheckResult::Current { latest, .. } => {
            println!(
                "latest {} ({}), {} assets, SHA256SUMS: {}",
                latest.version,
                latest.url,
                latest.assets.len(),
                latest.assets.iter().any(|a| a == "SHA256SUMS.txt")
            );
        }
        other => println!("{other:?}"),
    }
}
