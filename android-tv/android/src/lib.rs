//! Resumo for Android TV. The player itself is the desktop code in `../src`;
//! this crate builds it as the native library of a NativeActivity and adds
//! the Android glue in `platform.rs`.

#![allow(dead_code)] // the desktop window chrome (title bar, resize edges) is compiled but unused here

#[path = "../../src/app.rs"]
mod app;
#[path = "../../src/browser.rs"]
mod browser;
#[path = "../../src/crop.rs"]
mod crop;
#[path = "../../src/history.rs"]
mod history;
#[path = "../../src/lan.rs"]
mod lan;
mod platform;
mod readahead;
mod surface;
#[path = "../../src/osk.rs"]
mod osk;
#[path = "../../src/playlist.rs"]
mod playlist;
#[path = "../../src/store.rs"]
mod store;
#[path = "../../src/taskbar.rs"]
mod taskbar;
#[path = "../../src/theme.rs"]
mod theme;
#[path = "../../src/tv.rs"]
mod tv;
#[path = "../../src/video.rs"]
mod video;
#[path = "../../src/vlc.rs"]
mod vlc;
#[path = "../../src/zoom.rs"]
mod zoom;

use winit::platform::android::activity::AndroidApp;

pub const ICON_PNG: &[u8] = include_bytes!("../../assets/icon.png");

#[no_mangle]
fn android_main(app: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("Resumo"));
    std::panic::set_hook(Box::new(|info| log::error!("panic: {info}")));
    platform::init(&app);

    // With alpha: the video layer shows through where the window is clear.
    let options = eframe::NativeOptions {
        android_app: Some(app),
        viewport: egui::ViewportBuilder::default().with_transparent(true),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Resumo",
        options,
        Box::new(|cc| {
            let app = app::PlayerApp::new(cc, Vec::new())
                .map_err(|e| format!("libVLC {} could not start: {e}", vlc::version()))?;
            Ok(Box::new(app))
        }),
    );
    if let Err(e) = result {
        log::error!("Resumo stopped: {e}");
    }
}
