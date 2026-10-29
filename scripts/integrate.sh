#!/usr/bin/env bash
# Shared desktop integration for source and release installations.
set -euo pipefail

bin="$HOME/.local/bin"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
config="${XDG_CONFIG_HOME:-$HOME/.config}"
portal_dir="$config/xdg-desktop-portal"
key=org.freedesktop.impl.portal.FileChooser

# Keep the old chooser line beside our override so uninstall can restore it
# without discarding unrelated edits made after installation.
edit_preference() {
    local file="$1" mode="$2" tmp
    tmp="$(mktemp "${file}.XXXXXX")"
    awk -v mode="$mode" -v key="$key" '
        function add() { if (mode == "install" && !added) { print key "=kestrel"; added=1 } }
        /^# kestrel previous: / { if (mode == "remove") { sub(/^# kestrel previous: /, ""); print } else print; next }
        /^[[:space:]]*\[/ { if (preferred) add(); preferred=($0 ~ /^[[:space:]]*\[preferred\][[:space:]]*$/); seen=seen || preferred }
        preferred && $0 ~ "^[[:space:]]*" key "[[:space:]]*=" {
            if (mode == "install") {
                if ($0 !~ "=[[:space:]]*kestrel[[:space:]]*$") print "# kestrel previous: " $0
                add()
            } else if ($0 !~ "=[[:space:]]*kestrel[[:space:]]*$") print
            next
        }
        { print }
        END { if (mode == "install" && !seen) print "\n[preferred]"; add() }
    ' "$file" > "$tmp"
    cat "$tmp" > "$file"
    rm -f "$tmp"
}

refresh() {
    update-desktop-database "$data/applications" 2>/dev/null || true
    gtk-update-icon-cache -q "$data/icons/hicolor" 2>/dev/null || true
    busctl --user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus ReloadConfig >/dev/null 2>&1 || true
    # Restart only Kestrel's owned backend; avoid matching unrelated command lines.
    if command -v pgrep >/dev/null; then
        while read -r pid; do
            exe="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
            if [ "${exe% (deleted)}" = "$bin/kestrel" ]; then kill "$pid" 2>/dev/null || true; fi
        done < <(pgrep -f '^.*kestrel --portal$' || true)
    fi
    if ! systemctl --user try-restart xdg-desktop-portal.service 2>/dev/null; then
        # Non-systemd sessions activate the portal through D-Bus on the next call.
        # Match only the frontend, leaving screen sharing backend processes alone.
        pkill -u "$(id -u)" -f '^(/usr/(local/)?lib(exec)?/)?xdg-desktop-portal([[:space:]]|$)' 2>/dev/null || true
    fi
}

if [ "${1:-}" = --uninstall ]; then
    if [ -f "$data/kestrel/portal-configs" ]; then
        while IFS= read -r conf; do
            [ -f "$conf" ] || continue
            edit_preference "$conf" remove
        done < "$data/kestrel/portal-configs"
        rm -f "$data/kestrel/portal-configs"
    fi
    rm -f "$bin/kestrel" "$bin/kestrel-update" "$bin/kestrel-uninstall" \
        "$data/applications/kestrel.desktop" \
        "$data/icons/hicolor/scalable/apps/kestrel.svg" \
        "$data/xdg-desktop-portal/portals/kestrel.portal" \
        "$data/dbus-1/services/org.freedesktop.impl.portal.desktop.kestrel.service"
    refresh
    echo 'Kestrel uninstalled. Source checkout and app settings were kept.'
    exit 0
fi

mkdir -p "$portal_dir" "$data/xdg-desktop-portal/portals" "$data/dbus-1/services" "$data/kestrel"
cat > "$data/xdg-desktop-portal/portals/kestrel.portal" <<'EOP'
[portal]
DBusName=org.freedesktop.impl.portal.desktop.kestrel
Interfaces=org.freedesktop.impl.portal.FileChooser;
EOP
cat > "$data/dbus-1/services/org.freedesktop.impl.portal.desktop.kestrel.service" <<EOP
[D-BUS Service]
Name=org.freedesktop.impl.portal.desktop.kestrel
Exec="$bin/kestrel" --portal
EOP

IFS=: read -ra desktops <<< "${XDG_CURRENT_DESKTOP:-}"
IFS=: read -ra config_dirs <<< "${XDG_CONFIG_DIRS:-/etc/xdg}"
IFS=: read -ra data_dirs <<< "${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
conf="$portal_dir/portals.conf"
[ -z "${desktops[0]:-}" ] || conf="$portal_dir/${desktops[0],,}-portals.conf"
# Follow the portal's search order so an override retains the effective settings,
# including desktop-specific defaults for screen sharing and screenshots.
effective=""
for base in "$config" "${config_dirs[@]}" "$data" "${data_dirs[@]}"; do
    for desktop in "${desktops[@]}" ""; do
        name=portals.conf
        [ -z "$desktop" ] || name="${desktop,,}-portals.conf"
        candidate="$base/xdg-desktop-portal/$name"
        if [ -f "$candidate" ]; then effective="$candidate"; break 2; fi
    done
done
if [ -n "$effective" ]; then
    case "$effective" in
        "$portal_dir/"*) conf="$effective" ;;
        *) cp "$effective" "$conf" ;;
    esac
elif [ ! -f "$conf" ]; then
    printf '[preferred]\ndefault=gtk\n' > "$conf"
fi
edit_preference "$conf" install
if ! grep -Fxq "$conf" "$data/kestrel/portal-configs" 2>/dev/null; then
    printf '%s\n' "$conf" >> "$data/kestrel/portal-configs"
fi

# Absolute Exec works even when ~/.local/bin is absent from the launcher's PATH.
# Escape Desktop Entry reserved characters and literal field-code percent signs.
KESTREL_DESKTOP_EXE="$bin/kestrel" awk '
    BEGIN {
        path=ENVIRON["KESTREL_DESKTOP_EXE"]
        for (i=1; i<=length(path); i++) {
            c=substr(path,i,1)
            if (c == "%") out=out "%%"
            else if (c == "\\" || c == "\"" || c == "`" || c == "$") out=out "\\\\" c
            else out=out c
        }
    }
    /^Exec=/ { print "Exec=\"" out "\" %f"; next }
    { print }
' "$data/applications/kestrel.desktop" > "$data/applications/kestrel.desktop.tmp"
mv "$data/applications/kestrel.desktop.tmp" "$data/applications/kestrel.desktop"

helper="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/integrate.sh"
[ "$helper" = "$data/kestrel/integrate.sh" ] || install -m755 "$helper" "$data/kestrel/integrate.sh"
printf '#!/usr/bin/env bash\nexec bash %q --uninstall\n' "$data/kestrel/integrate.sh" > "$bin/kestrel-uninstall"
chmod 755 "$bin/kestrel-uninstall"
refresh
echo 'File dialogs configured for apps using xdg-desktop-portal. Reopen apps to use them.'
