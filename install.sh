#!/bin/sh
# keytab installer for Linux (niri):
#   curl -fsSL https://raw.githubusercontent.com/HayzDev/keytab/main/install.sh | sh
# Installs keytab and keytabctl to ~/.local/bin, adds the niri keybinds, and starts keytab.
# Run it from an extracted release archive to install those files instead of downloading.
set -eu

bin="$HOME/.local/bin"
niri_dir="${XDG_CONFIG_HOME:-$HOME/.config}/niri"
here=$(dirname "$0")

if [ -f "$here/keytab" ] && [ -f "$here/keytabctl" ]; then
    src=$here
else
    [ "$(uname -m)" = x86_64 ] || { echo "keytab: prebuilt binaries are x86_64 only; build from source" >&2; exit 1; }
    src=$(mktemp -d)
    trap 'rm -rf "$src"' EXIT
    echo "Downloading keytab..."
    curl -fsSL https://github.com/HayzDev/keytab/releases/latest/download/keytab-linux-x86_64.tar.gz |
        tar -xz -C "$src" --strip-components=1
fi

"$bin/keytabctl" --quit 2>/dev/null || true
install -Dm755 "$src/keytab" "$bin/keytab"
install -Dm755 "$src/keytabctl" "$bin/keytabctl"
install -Dm644 "$src/niri.kdl" "$niri_dir/keytab.kdl"

config="$niri_dir/config.kdl"
if [ -f "$config" ] && ! grep -q 'include "keytab.kdl"' "$config"; then
    printf '\ninclude "keytab.kdl"\n' >> "$config"
    echo "Added include \"keytab.kdl\" to $config"
fi

if [ -n "${NIRI_SOCKET:-}" ] && command -v niri >/dev/null; then
    niri msg action load-config-file >/dev/null 2>&1 || true
    nohup "$bin/keytab" >/dev/null 2>&1 &
    echo "keytab is running. Press Alt+Tab or Mod+Tab (add Shift for search)."
else
    echo "Installed. Log into niri (keytab starts automatically), then press Alt+Tab or Mod+Tab."
fi
