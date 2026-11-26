// Windows-only: embed the app icon and version metadata in the .exe.
// Inert on other hosts (winres is a Windows-only build-dependency).
//
// Also records the short git revision so Settings shows which build is
// running: a stale binary was mistaken for a source bug more than once.

use std::process::Command;

fn git_hash() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let hash = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if hash.is_empty() || hash.len() > 40 {
        None
    } else {
        Some(hash)
    }
}

fn main() {
    println!("cargo:rerun-if-changed=../.git/HEAD");
    if let Some(hash) = git_hash() {
        println!("cargo:rustc-env=ARIADNE_GIT_HASH={hash}");
    }
    #[cfg(windows)]
    windows_metadata();
}

#[cfg(windows)]
fn windows_metadata() {
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    res.set("FileDescription", "Ariadne writing workspace");
    res.set("ProductName", "Ariadne");
    if let Err(e) = res.compile() {
        eprintln!("winres failed (non-fatal): {e}");
    }
}
