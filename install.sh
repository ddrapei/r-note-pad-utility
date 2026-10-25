#!/usr/bin/env bash
# Builds rnote-pad and installs it for the current user. The udev rule needs
# root, so it is printed rather than installed.
set -euo pipefail
cd "$(dirname "$0")"

mise exec -- cargo build --release
install -Dm755 target/release/rnote-pad ~/.local/bin/rnote-pad
install -Dm644 rnote-pad.service ~/.config/systemd/user/rnote-pad.service
if [[ ! -e ~/.config/rnote-pad/config.toml ]]; then
  install -Dm644 config.toml ~/.config/rnote-pad/config.toml
fi
systemctl --user daemon-reload
systemctl --user try-restart rnote-pad.service

if ! cmp -s 70-rnote-pad.rules /etc/udev/rules.d/70-rnote-pad.rules; then
  echo
  echo "The udev rule is missing or out of date. As root:"
  echo "  sudo install -m644 $PWD/70-rnote-pad.rules /etc/udev/rules.d/ && sudo udevadm control --reload"
  echo "then turn the tablet off and on again."
fi
