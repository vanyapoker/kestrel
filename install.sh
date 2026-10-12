#!/usr/bin/env bash
# Installs or updates Kestrel for the current user.
#
#   curl -fsSL https://raw.githubusercontent.com/vanyapoker/kestrel/master/install.sh | bash
#
# Run from a clone it builds that clone; otherwise it clones the repo to
# ~/.local/share/kestrel/source. Either way it installs the binary, launcher
# entry and icon, plus a `kestrel-update` command that pulls and rebuilds.
#
#   install.sh             install / rebuild
#   install.sh --update    git pull first, then rebuild (what kestrel-update runs)
#   install.sh --uninstall remove everything this script installed
set -euo pipefail

REPO_URL="${KESTREL_REPO:-https://github.com/vanyapoker/kestrel.git}"

BIN="$HOME/.local/bin"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
MANAGED_SRC="$DATA/kestrel/source"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

uninstall() {
    rm -f "$BIN/kestrel" "$BIN/kestrel-update" \
        "$DATA/applications/kestrel.desktop" \
        "$DATA/icons/hicolor/scalable/apps/kestrel.svg"
    rm -rf "$DATA/kestrel"
    update-desktop-database "$DATA/applications" 2>/dev/null || true
    say "Kestrel uninstalled"
}

# Rust, git and a C toolchain (for the linker). Omarchy ships everything else.
ensure_deps() {
    [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
    local missing=()
    command -v git >/dev/null || missing+=(git)
    command -v cc >/dev/null || missing+=(base-devel)
    command -v cargo >/dev/null || missing+=(rustup)
    if [ ${#missing[@]} -gt 0 ]; then
        command -v pacman >/dev/null || die "please install: ${missing[*]} (and a Rust toolchain via rustup)"
        say "Installing build dependencies: ${missing[*]}"
        sudo pacman -S --needed --noconfirm "${missing[@]}"
    fi
    if ! cargo --version >/dev/null 2>&1; then
        say "Installing the stable Rust toolchain"
        rustup default stable
    fi
}

# Picks the source tree: the clone this script lives in, or the managed clone.
find_source() {
    local here=""
    # Piped into bash there is no script file, so always use the managed clone.
    if [ -f "${BASH_SOURCE[0]:-}" ]; then
        here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    fi
    if [ -n "$here" ] && [ -f "$here/Cargo.toml" ] && grep -q '^name = "kestrel"' "$here/Cargo.toml"; then
        SRC="$here"
    else
        SRC="$MANAGED_SRC"
        if [ ! -d "$SRC/.git" ]; then
            say "Downloading Kestrel"
            mkdir -p "$(dirname "$SRC")"
            git clone --depth 1 "$REPO_URL" "$SRC"
            return
        fi
        UPDATE=1
    fi
}

UPDATE=0
case "${1:-}" in
    --uninstall) uninstall; exit 0 ;;
    --update) UPDATE=1 ;;
    "") ;;
    *) die "unknown option: $1 (use --update or --uninstall)" ;;
esac

ensure_deps
find_source
cd "$SRC"

if [ "$UPDATE" = 1 ] && [ -d .git ]; then
    say "Pulling the latest version"
    old="$(git rev-parse HEAD)"
    git pull --ff-only
    [ "$old" = "$(git rev-parse HEAD)" ] && say "Already up to date; rebuilding anyway"
fi

say "Building (the first build takes a few minutes)"
cargo build --release --locked

install -Dm755 target/release/kestrel "$BIN/kestrel"
install -Dm644 assets/kestrel.svg "$DATA/icons/hicolor/scalable/apps/kestrel.svg"
install -Dm644 assets/kestrel.desktop "$DATA/applications/kestrel.desktop"
cat > "$BIN/kestrel-update" <<EOF
#!/bin/sh
# Updates Kestrel from $SRC
exec bash "$SRC/install.sh" --update
EOF
chmod 755 "$BIN/kestrel-update"

# The app used to be called fpilot; drop the old copies so the launcher shows one entry.
rm -f "$BIN/fpilot" "$DATA/applications/fpilot.desktop" \
    "$DATA/icons/hicolor/scalable/apps/fpilot.svg"
update-desktop-database "$DATA/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$DATA/icons/hicolor" 2>/dev/null || true

say "Installed Kestrel $(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1) to $BIN/kestrel"
case ":$PATH:" in
    *":$BIN:"*) ;;
    *) echo "   Note: $BIN is not on your PATH; launch Kestrel from the app launcher." ;;
esac

optional=()
command -v pdftoppm >/dev/null || optional+=(poppler)
command -v ffmpeg >/dev/null || optional+=(ffmpeg)
command -v rsvg-convert >/dev/null || optional+=(librsvg)
command -v wl-paste >/dev/null || optional+=(wl-clipboard)
if [ ${#optional[@]} -gt 0 ]; then
    echo "   For PDF/video/SVG previews and pasting from other apps: sudo pacman -S ${optional[*]}"
fi
