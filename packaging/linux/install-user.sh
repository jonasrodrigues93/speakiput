#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
project_root=$(dirname -- "$(dirname -- "$script_dir")")
cd "$project_root"

install -Dm755 target/release/speakiput "$HOME/.local/bin/speakiput"
install -Dm755 target/release/speakiputd "$HOME/.local/bin/speakiputd"
install -Dm644 packaging/linux/speakiput.desktop \
    "$HOME/.local/share/applications/io.github.jonas.speakiput.desktop"
install -Dm644 packaging/linux/speakiputd-user.service \
    "$HOME/.config/systemd/user/speakiputd.service"
install -Dm644 packaging/linux/autostart/speakiput.desktop \
    "$HOME/.config/autostart/speakiput.desktop"
install -Dm644 apps/speakiput/assets/tray-icon.svg \
    "$HOME/.local/share/icons/hicolor/scalable/apps/io.github.jonas.speakiput.svg"

# Desktop-session PATHs do not consistently include ~/.local/bin. Keep a
# user-only installation self-contained instead of accidentally launching a
# second system-wide copy from /usr/bin.
sed -i "s|^Exec=speakiput|Exec=$HOME/.local/bin/speakiput|" \
    "$HOME/.local/share/applications/io.github.jonas.speakiput.desktop" \
    "$HOME/.config/autostart/speakiput.desktop"

printf '%s\n' "Installed speakiput for $USER under $HOME/.local"
printf '%s\n' "Enable the backend with: systemctl --user enable --now speakiputd.service"
