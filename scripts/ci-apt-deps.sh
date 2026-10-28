#!/usr/bin/env bash
# System packages the workspace needs to build on the Ubuntu CI runner: the desktop crate
# (GTK / WebKitGTK / tray / xdo) and the TLS and D-Bus bindings behind the OS keyring.

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
  librsvg2-dev
