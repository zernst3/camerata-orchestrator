#!/usr/bin/env bash
# System packages the workspace needs to build on the Ubuntu CI runner: the desktop crate
# (GTK / WebKitGTK / tray / xdo) and the TLS and D-Bus bindings behind the OS keyring, plus
# CJK and emoji fonts so typst can render the multi-byte PDF text tests the way macOS does.

set -euo pipefail

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  pkg-config \
  libssl-dev \
  libdbus-1-dev \
  libgtk-3-dev \
  libwebkit2gtk-4.1-dev \
  libxdo-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  fonts-noto-cjk \
  fonts-noto-color-emoji
