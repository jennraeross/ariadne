// Windows-only: embed the app icon and version metadata in the .exe.
// Inert on other hosts (winres is a Windows-only build-dependency).
#[cfg(windows)]
fn main() {
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    res.set("FileDescription", "Ariadne writing workspace");
    res.set("ProductName", "Ariadne");
    if let Err(e) = res.compile() {
        eprintln!("winres failed (non-fatal): {e}");
    }
}

#[cfg(not(windows))]
fn main() {}
