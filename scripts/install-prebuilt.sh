#!/usr/bin/env bash
# Install the files from an extracted Kestrel release; no build tools required.
set -euo pipefail

if [ "$#" -ne 0 ]; then
    echo "Usage: ./install.sh" >&2
    exit 1
fi

bundle_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
version="$(cat "$bundle_dir/VERSION")"

install -Dm755 "$bundle_dir/kestrel" "$HOME/.local/bin/kestrel"
install -Dm644 "$bundle_dir/assets/kestrel.svg" "$data_dir/icons/hicolor/scalable/apps/kestrel.svg"
install -Dm644 "$bundle_dir/assets/kestrel.desktop" "$data_dir/applications/kestrel.desktop"
update-desktop-database "$data_dir/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$data_dir/icons/hicolor" 2>/dev/null || true

echo "Installed Kestrel $version to $HOME/.local/bin/kestrel"
case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) echo "Add $HOME/.local/bin to PATH, or open Kestrel from the app launcher." ;;
esac
