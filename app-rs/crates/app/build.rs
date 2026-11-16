//! Windows only: links the app icon (packaging/windows/wraithgrid.rc) into wraithgrid.exe.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let dir = std::path::Path::new("../../packaging/windows");
    println!(
        "cargo:rerun-if-changed={}",
        dir.join("wraithgrid.rc").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dir.join("wraithgrid.ico").display()
    );
    embed_resource::compile(
        dir.join("wraithgrid.rc"),
        embed_resource::ParamsIncludeDirs([dir]),
    )
    .manifest_optional()
    .expect("compile the Windows icon resource");
}
