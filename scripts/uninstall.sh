#!/usr/bin/env bash
set -euo pipefail

sudo systemctl disable --now nuc-powerd.service || true
sudo rm -f /etc/systemd/system/nuc-powerd.service
sudo rm -f /usr/local/bin/nuc-powerd
sudo rm -f /usr/local/bin/nuc-powerd-ui
sudo rm -f /etc/nuc-powerd.toml
sudo rm -rf /run/nuc-powerd
sudo systemctl daemon-reload

echo "Removed nuc-powerd service and files"
