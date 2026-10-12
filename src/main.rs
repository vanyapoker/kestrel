mod app;
mod fsops;
mod preview;
mod theme;
mod util;

use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result {
    let start = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .and_then(|p| p.canonicalize().ok())
        .filter(|p| p.is_dir())
        .unwrap_or_else(fsops::home_dir);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("fpilot")
            .with_app_id("fpilot")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "fpilot",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, start)))),
    )
}
