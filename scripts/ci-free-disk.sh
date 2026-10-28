#!/usr/bin/env bash
# A standard GitHub runner has limited free disk, and a full workspace build with debug info
# is large. Remove big preinstalled toolchains this build never uses.

set -uo pipefail

echo "before:"
df -h /

sudo rm -rf /usr/local/lib/android /usr/share/dotnet /opt/ghc /usr/local/.ghcup /opt/hostedtoolcache/CodeQL
sudo docker image prune --all --force || true

echo "after:"
df -h /
