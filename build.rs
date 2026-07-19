fn main() {
    // Embed the application icon into the Windows PE resource table so Explorer /
    // taskbar show it for the .exe (in addition to the runtime window icon).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().expect("embed Windows application icon");
    }

    println!("cargo:rerun-if-changed=assets/icon.png");
    println!("cargo:rerun-if-changed=assets/icon.ico");
}
