#!/bin/sh
# Builds fpilot and installs it for the current user (launcher entry + icon).
set -e
cd "$(dirname "$0")"
cargo build --release
install -Dm755 target/release/fpilot "$HOME/.local/bin/fpilot"
install -Dm644 assets/fpilot.svg "$HOME/.local/share/icons/hicolor/scalable/apps/fpilot.svg"
install -Dm644 assets/fpilot.desktop "$HOME/.local/share/applications/fpilot.desktop"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
echo "Installed fpilot to $HOME/.local/bin/fpilot"
