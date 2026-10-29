# Kestrel

A keyboard-friendly file manager for [Omarchy](https://omarchy.org), inspired by [File Pilot](https://filepilot.tech).

[![Kestrel with split panes and image previews](assets/screenshots/overview.png)](assets/screenshots/overview.png)

- Tabs, split panes, and Details, List, Icons, and Miller column views.
- Fuzzy filtering, recursive search, bookmarks, and saved workspaces.
- Previews for images, code, PDFs, videos, tables, and archives.
- Batch rename, copy and move with conflict handling, trash, and undo.
- Live folder sizes, removable drives, and camera and phone support.
- Omarchy theme colors and Open / Save dialogs for portal-enabled apps.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/vanyapoker/kestrel/master/install.sh | bash
```

The installer builds Kestrel and installs the app, launcher, icon, and file-dialog
backend for your user. On Arch / Omarchy it installs missing build tools too.
The first build takes a few minutes. Open **Kestrel** from your launcher or run
`kestrel ~/Downloads`.

Update with `kestrel-update`. Remove with `kestrel-uninstall`.

**Prefer a prebuilt binary?** Download and extract the Linux x86_64 archive from
[Releases](https://github.com/vanyapoker/kestrel/releases), then run its `./install.sh`.
It includes the same desktop and file-dialog integration. Check `BUILD-INFO.txt`
for glibc requirements; build from source on older systems. Update by installing
the next release.

## Shortcuts

| Key | Action |
|---|---|
| Start typing / Esc | Filter folder / clear filter |
| Ctrl+P / Ctrl+K | Command palette / go to folder or tab |
| Ctrl+L | Edit path (Tab completes folders) |
| Enter / Backspace | Open / parent folder |
| Alt+← / Alt+→ / Alt+Home | Back / forward / home |
| Ctrl+F / Ctrl+Shift+F | Recursive search / flatten subfolders |
| Ctrl+T / Ctrl+W / Ctrl+Shift+T | New / close / reopen tab or split |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |
| Ctrl+1…9 | Switch to tab |
| F6 or Ctrl+\ / Tab | Toggle split / switch pane |
| Ctrl+G / Ctrl+Shift+M | Icons and details / Miller columns |
| Space or F3 | Preview |
| Ctrl+H / Ctrl+B | Hidden files / sidebar |
| Ctrl+scroll | View size |
| Ctrl+C / Ctrl+X / Ctrl+V | Copy / cut / paste |
| Ctrl+Z / Ctrl+D | Undo / duplicate |
| Ctrl+A / Ctrl+Space | Select all / toggle selected item |
| Ctrl+S / Ctrl+Shift+A | Select by pattern / invert selection |
| F2 / Ctrl+Shift+N | Rename (batch for multiple items) / new folder |
| Delete / Shift+Delete | Trash / permanently delete |
| Alt+C / Ctrl+Alt+T | Copy path / open terminal here |
| F5 | Refresh |

Use Shift+arrows or Ctrl/Shift+click for multiple selection. In Miller columns,
←/→ moves between parent and child columns. Shortcuts leave typing and cursor
movement in text fields alone. The command palette lists more actions.

In file dialogs, **Ctrl+Enter** confirms, **Ctrl+L** edits the path, and **Esc**
clears a filter or cancels. Saving over an existing file asks for confirmation.

## Desktop integration

Installation selects Kestrel for Open / Save dialogs in apps using
xdg-desktop-portal, preserving other portal backends and restoring the previous
chooser setting on uninstall. Reopen apps after installation.
Apps with their own file pickers keep them. For Firefox, set
`widget.use-xdg-desktop-portal.file-picker` to `1` in `about:config` if needed.

To make Kestrel your default file manager:

```sh
xdg-mime default kestrel.desktop inode/directory
```

Omarchy includes the usual preview helpers. On other systems, install `poppler`,
`ffmpeg`, `librsvg`, and `wl-clipboard` for PDF/video/SVG previews and clipboard
support. Archives use `bsdtar`; drives use `udisksctl`; cameras and phones use
`gvfs-gphoto2` and `gvfs-mtp`.

## Build

```sh
git clone https://github.com/vanyapoker/kestrel.git
cd kestrel
./install.sh          # build and install this checkout
cargo run --release   # run without installing
cargo test --locked
```

Written in Rust with [egui](https://github.com/emilk/egui). The vendored
[egui-winit patches](vendor/egui-winit/KESTREL_PATCHES.md) support drag and drop
on Hyprland. See [advanced usage](docs/usage.md) for custom actions, shell
pickers, and release packaging.

[MIT](LICENSE)
