//! Tells the linker where libVLC lives when it is not on the default path.
//!
//! Set `VLC_LIB_DIR` to the folder with `libvlc.lib` (Windows SDK),
//! `libvlc.dylib` (macOS) or `libvlc.so` (Linux) to override the defaults.

fn main() {
    println!("cargo:rerun-if-env-changed=VLC_LIB_DIR");
    println!("cargo:rerun-if-changed=assets/resumo.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Gives resumo.exe its icon in Explorer and the taskbar.
        if let Err(e) = embed_resource::compile("assets/resumo.rc", embed_resource::NONE).manifest_optional() {
            println!("cargo:warning=could not embed the Windows icon: {e}");
        }
    }
    if let Ok(dir) = std::env::var("VLC_LIB_DIR") {
        println!("cargo:rustc-link-search=native={dir}");
        return;
    }
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let candidates: &[&str] = match target.as_str() {
        "macos" => &["/Applications/VLC.app/Contents/MacOS/lib"],
        "windows" => &["C:\\Program Files\\VideoLAN\\VLC\\sdk\\lib", "C:\\Program Files\\VideoLAN\\VLC"],
        _ => &[],
    };
    for dir in candidates {
        if std::path::Path::new(dir).exists() {
            println!("cargo:rustc-link-search=native={dir}");
            if target == "macos" {
                println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
            }
        }
    }
}
