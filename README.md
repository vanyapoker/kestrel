# fpilot

A fast, keyboard-friendly file manager for Linux, inspired by File Pilot.
Rust + egui (OpenGL), all filesystem work happens off the UI thread.

    cargo run --release -- [path]

## Keys

| Key | Action |
|---|---|
| ↑ ↓ PgUp PgDn Home End | Move selection |
| Enter / double-click | Open (folder or default app) |
| Backspace / Alt+↑ | Up (or delete last filter char) |
| Alt+← / Alt+→, mouse back/forward | History |
| *type anything* | Filter current folder, Esc clears |
| Ctrl+L / click path bar | Edit path (`~` works) |
| Ctrl+T / Ctrl+W | New / close tab (middle-click also works) |
| Ctrl+Tab, Ctrl+1..9 | Switch tabs |
| Ctrl+H | Toggle hidden files |
| F3 | Toggle preview panel |
| F5 | Refresh |

Benchmark: `cargo test --release -- --ignored --nocapture bench`
