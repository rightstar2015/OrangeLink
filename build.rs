fn main() {
    println!("cargo:rerun-if-changed=assets/orangelink.rc");
    println!("cargo:rerun-if-changed=assets/orangelink.ico");
    // Embed the icon so Explorer and the taskbar show it for the .exe.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let _ = embed_resource::compile("assets/orangelink.rc", embed_resource::NONE);
    }
}
