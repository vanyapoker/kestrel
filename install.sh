#!/bin/sh
# Builds kestrel and installs it for the current user (launcher entry + icon).
set -e
cd "$(dirname "$0")"
cargo build --release
install -Dm755 target/release/kestrel "$HOME/.local/bin/kestrel"
install -Dm644 assets/kestrel.svg "$HOME/.local/share/icons/hicolor/scalable/apps/kestrel.svg"
install -Dm644 assets/kestrel.desktop "$HOME/.local/share/applications/kestrel.desktop"
# The app used to be called fpilot; drop the old copies so the launcher shows one entry.
rm -f "$HOME/.local/bin/fpilot" "$HOME/.local/share/applications/fpilot.desktop" \
    "$HOME/.local/share/icons/hicolor/scalable/apps/fpilot.svg"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
echo "Installed kestrel to $HOME/.local/bin/kestrel"
