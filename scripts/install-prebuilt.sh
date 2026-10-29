#!/usr/bin/env bash
# Install the files from an extracted Kestrel release; no build tools required.
set -euo pipefail

if [ "${1:-}" = --uninstall ]; then
    exec bash "$(dirname -- "${BASH_SOURCE[0]}")/scripts/integrate.sh" --uninstall
fi

if [ "$#" -ne 0 ]; then
    echo "Usage: ./install.sh" >&2
    exit 1
fi

bundle_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
version="$(cat "$bundle_dir/VERSION")"

install -Dm755 "$bundle_dir/kestrel" "$HOME/.local/bin/kestrel"
rm -f "$HOME/.local/bin/kestrel-update"
install -Dm644 "$bundle_dir/assets/kestrel.svg" "$data_dir/icons/hicolor/scalable/apps/kestrel.svg"
install -Dm644 "$bundle_dir/assets/kestrel.desktop" "$data_dir/applications/kestrel.desktop"
bash "$bundle_dir/scripts/integrate.sh"

echo "Installed Kestrel $version to $HOME/.local/bin/kestrel"
case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) echo "Add $HOME/.local/bin to PATH, or open Kestrel from the app launcher." ;;
esac
