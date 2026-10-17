//! VPlayer: a simple modern video player on top of libVLC.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod crop;
mod playlist;
mod store;
mod taskbar;
mod theme;
mod video;
mod vlc;

use std::path::PathBuf;

pub const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

fn main() -> eframe::Result {
    let files: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("VPlayer")
            .with_app_id("vplayer")
            .with_inner_size([1180.0, 700.0])
            .with_min_inner_size([640.0, 380.0])
            .with_decorations(false)
            .with_icon(eframe::icon_data::from_png_bytes(ICON_PNG).expect("embedded icon is a valid PNG"))
            .with_drag_and_drop(true),
        ..Default::default()
    };

    eframe::run_native(
        "VPlayer",
        options,
        Box::new(move |cc| {
            let app = app::PlayerApp::new(cc, files)
                .map_err(|e| format!("libVLC {} could not start: {e}. Is VLC installed?", vlc::version()))?;
            Ok(Box::new(app))
        }),
    )
}
