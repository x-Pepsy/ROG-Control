#!/usr/bin/env bash
# Installa ROG Control: binari, demone systemd, voce di menu e icona.
set -euo pipefail

cd "$(dirname "$0")"

SUDO=""
if [[ $EUID -ne 0 ]]; then
    SUDO="sudo"
fi

echo "==> Compilazione release"
cargo build --release

echo "==> Installazione binari in /usr/local/bin"
$SUDO install -Dm755 target/release/rog-control /usr/local/bin/rog-control
$SUDO install -Dm755 target/release/rog-control-daemon /usr/local/bin/rog-control-daemon

echo "==> Installazione unit systemd"
$SUDO install -Dm644 packaging/rog-control-daemon.service /etc/systemd/system/rog-control-daemon.service

echo "==> Installazione voce di menu e icona"
$SUDO install -Dm644 packaging/rog-control.desktop /usr/share/applications/rog-control.desktop
for size in 16 24 32 48 64 128 256 512; do
    $SUDO install -Dm644 "packaging/icons/rog-control-${size}.png" \
        "/usr/share/icons/hicolor/${size}x${size}/apps/rog-control.png"
done
$SUDO gtk-update-icon-cache -f -t /usr/share/icons/hicolor >/dev/null 2>&1 || true

echo "==> Documentazione"
$SUDO install -Dm644 README.md /usr/local/share/doc/rog-control/README.md

echo "==> Attivazione demone"
$SUDO systemctl daemon-reload
$SUDO systemctl enable --now rog-control-daemon.service
$SUDO systemctl --no-pager --lines=0 status rog-control-daemon.service || true

echo
echo "Fatto. Avvia 'ROG Control' dal menu applicazioni oppure esegui: rog-control"
