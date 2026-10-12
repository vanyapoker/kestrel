mod app;
mod complete;
mod fsops;
mod ops;
mod palette;
mod preview;
mod search;
mod tab;
mod theme;
mod thumbs;
mod util;
mod watch;
mod wl_dnd;

use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result {
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => {
            println!("kestrel {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--help" | "-h") => {
            println!("Usage: kestrel [FOLDER]\n\nUpdate with `kestrel-update`.");
            return Ok(());
        }
        _ => {}
    }
    let start = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .and_then(|p| p.canonicalize().ok())
        .filter(|p| p.is_dir())
        .unwrap_or_else(fsops::home_dir);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kestrel")
            .with_app_id("kestrel")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0]),
        // With vsync, Mesa blocks in eglSwapBuffers until the compositor's frame
        // callback, which never comes while the window sits on a hidden workspace,
        // so the app froze there. Frames are paced by `pre_present_notify` instead.
        glow_options: eframe::egui_glow::GlowConfiguration { vsync: false, ..Default::default() },
        ..Default::default()
    };
    eframe::run_native(
        "kestrel",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, start)))),
    )
}
