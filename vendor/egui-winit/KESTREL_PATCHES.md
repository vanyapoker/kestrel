# Kestrel patches to egui-winit 0.36.2

1. `src/clipboard.rs` `init_smithay_clipboard`: return `None` (unless
   `KESTREL_SMITHAY_CLIPBOARD` is set). smithay-clipboard creates the client's
   first `wl_data_device`, and Hyprland delivers drag-and-drop events only to
   the first one, so kestrel's own data device (src/wl_dnd.rs) would never see
   drops. Text clipboard falls back to arboard.
2. `src/lib.rs` paste handling: always emit `Event::Paste`, even when the
   clipboard has no text, so Ctrl+V reaches the app when files were copied in
   another file manager.

When upgrading eframe, re-copy egui-winit from the registry and re-apply.
