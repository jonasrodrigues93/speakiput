#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
project_root=$(dirname -- "$(dirname -- "$script_dir")")
cd "$project_root"

prefix=${PREFIX:-/usr}
destination=${DESTDIR:-}

install -Dm755 target/release/speakiput "$destination$prefix/bin/speakiput"
install -Dm755 target/release/speakiputd "$destination$prefix/bin/speakiputd"
install -Dm644 packaging/linux/speakiput.desktop \
    "$destination$prefix/share/applications/io.github.jonas.speakiput.desktop"
install -Dm644 packaging/linux/speakiputd.service \
    "$destination$prefix/lib/systemd/user/speakiputd.service"
install -Dm644 packaging/linux/autostart/speakiput.desktop \
    "$destination/etc/xdg/autostart/speakiput.desktop"
install -Dm644 apps/speakiput/assets/tray-icon.svg \
    "$destination$prefix/share/icons/hicolor/scalable/apps/io.github.jonas.speakiput.svg"
install -Dm644 packaging/linux/99-speakiput.rules \
    "$destination$prefix/lib/udev/rules.d/99-speakiput.rules"

printf '%s\n' "Installed speakiput under $destination$prefix"
printf '%s\n' "Enable the backend with: systemctl --user enable --now speakiputd.service"
