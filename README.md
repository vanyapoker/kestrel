# fpilot

A fast, keyboard-friendly file manager for Linux, inspired by File Pilot.
Rust + egui (OpenGL); all filesystem work happens off the UI thread.

    cargo run --release -- [path]

Features: tabs, split view, details/list/icon views (Ctrl+wheel to zoom),
hideable sidebar and preview panel, recursive search,
command palette, background copy/move with progress (uses `copy_file_range`,
so copies on btrfs/xfs are instant reflinks), trash, drag and drop, live
folder sizes, auto-refresh on changes, and previews for images, code/text,
PDF, video, SVG and audio metadata.

Optional tools for previews: `pdftoppm`/`pdfinfo` (poppler), `ffmpeg`/`ffprobe`,
`rsvg-convert`. Pasting files copied in other apps uses `wl-paste`.

## Keys

| Key | Action |
|---|---|
| Ctrl+P | Command palette |
| ↑ ↓ PgUp PgDn Home End (← → in grid) | Move cursor; with Shift, extend selection |
| Ctrl+click, Shift+click, Ctrl+Space, Ctrl+A | Multi-select |
| Enter / double-click | Open (folder or default app) |
| Backspace / Alt+↑ | Up (or delete last filter char) |
| Alt+← / Alt+→, mouse back/forward | History |
| *type anything* | Filter current folder; Esc clears |
| Ctrl+F | Recursive search in current folder |
| Ctrl+C / Ctrl+X / Ctrl+V | Copy / cut / paste files |
| Ctrl+D | Duplicate |
| F2 | Rename |
| Ctrl+Shift+N | New folder |
| Delete / Shift+Delete | Move to trash / delete permanently |
| Alt+C | Copy path |
| Ctrl+Alt+T | Open terminal here |
| Ctrl+L / click path bar | Edit path (`~` works); folders autocomplete: Tab completes, ↑ ↓ pick, Enter goes |
| Ctrl+T / Ctrl+W | New / close tab (middle-click also works) |
| Ctrl+Tab, Ctrl+1..9 | Switch tabs |
| F6 or Ctrl+\\ | Toggle split view; Tab switches pane |
| Ctrl+G | Toggle icons / details view |
| Ctrl+wheel | Zoom: details → list → small icons → … → huge icons |
| Ctrl+H | Toggle hidden files |
| F3 | Toggle preview panel |
| Ctrl+B | Toggle sidebar (both panels can also be dragged shut) |
| F5 | Refresh |

Sidebar sections fold shut by clicking their header. Bookmark a folder from
its right-click menu, the palette, or by dragging it onto the Bookmarks
section. Bookmarks are GTK's (`~/.config/gtk-3.0/bookmarks`), so they're shared
with Nautilus and file pickers.

Drag files onto folders, tabs, breadcrumbs or the sidebar to move them
(Ctrl to copy; across filesystems it always copies). Name clashes never
overwrite: the new item gets a " (2)" suffix.

Benchmark: `cargo test --release -- --ignored --nocapture bench`

## Install

    ./install.sh

Builds a release binary into `~/.local/bin/fpilot` and adds an icon and a
launcher entry (`~/.local/share/applications/fpilot.desktop`). Re-run it after
pulling changes.
