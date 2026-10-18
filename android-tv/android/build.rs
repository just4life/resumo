//! Points the linker at libVLC for Android: `VLC_LIB_DIR` is the folder with
//! `libvlc.so` and `libc++_shared.so` (`jni/arm64-v8a` of the libvlc-all AAR).

fn main() {
    println!("cargo:rerun-if-env-changed=VLC_LIB_DIR");
    match std::env::var("VLC_LIB_DIR") {
        Ok(dir) => println!("cargo:rustc-link-search=native={dir}"),
        Err(_) => println!("cargo:warning=VLC_LIB_DIR is not set; linking libvlc.so may fail"),
    }
}
