mod actions;
mod app;
mod apps;
mod complete;
mod devices;
mod fsops;
mod ops;
mod palette;
mod picker;
mod portal;
mod preview;
mod props;
mod rename;
mod search;
mod session;
mod sizes;
mod tab;
mod theme;
mod thumb_cache;
mod thumbs;
mod util;
mod watch;
mod wl_dnd;
mod workspaces;

use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result {
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => {
            println!("kestrel {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--help" | "-h") => {
            println!(
                "Usage: kestrel [FOLDER]\n\nUpdates: https://github.com/vanyapoker/kestrel/releases/latest\nSource installations can also run `kestrel-update`."
            );
            return Ok(());
        }
        Some("--portal") => {
            if let Err(e) = portal::run() {
                eprintln!("kestrel: portal backend failed: {e}");
                std::process::exit(1);
            }
            return Ok(());
        }
        _ => {}
    }
    let args: Vec<String> = std::env::args().collect();
    let picker = picker::parse_args(&args);
    // With a folder argument, open just that folder; otherwise restore the last session.
    let start = std::env::args_os()
        .nth(1)
        .filter(|_| picker.is_none())
        .map(PathBuf::from)
        .and_then(|p| p.canonicalize().ok())
        .filter(|p| p.is_dir());
    // A picker is a throwaway window: it neither restores nor overwrites the session.
    let session = if picker.is_some() {
        None
    } else {
        session::load()
    };
    let (title, app_id, size) = match &picker {
        Some(p) => (p.heading(), "kestrel-picker", [980.0, 640.0]),
        None => ("Kestrel".to_string(), "kestrel", [1280.0, 800.0]),
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_app_id(app_id)
            .with_inner_size(size)
            .with_min_inner_size([640.0, 400.0]),
        // With vsync, Mesa blocks in eglSwapBuffers until the compositor's frame
        // callback, which never comes while the window sits on a hidden workspace,
        // so the app froze there. Frames are paced by `pre_present_notify` instead.
        glow_options: eframe::egui_glow::GlowConfiguration {
            vsync: false,
            ..Default::default()
        },
        ..Default::default()
    };
    eframe::run_native(
        "kestrel",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, start, session, picker)))),
    )
}
