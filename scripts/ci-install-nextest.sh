#!/usr/bin/env bash
# Install a pinned cargo-nextest release binary on the CI runner.
#
# The build job archives the test binaries with it and the partition jobs run them from the
# archive, so every job must run the same nextest. The archive is checked against a pinned
# SHA-256 before it is unpacked, the same way scripts/ci-install-typst.sh pins typst. Bump the
# version and the checksum together.

set -euo pipefail

NEXTEST_VERSION="0.9.152"
NEXTEST_ARCHIVE="cargo-nextest-${NEXTEST_VERSION}-x86_64-unknown-linux-gnu.tar.gz"
NEXTEST_SHA256="d039b43c3e0958441b6ad0a39c8884d018b3d2e8fb9928f209074b1100b5e17a"

work="$(mktemp -d)"
curl -fsSL --retry 3 -o "${work}/${NEXTEST_ARCHIVE}" \
  "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-${NEXTEST_VERSION}/${NEXTEST_ARCHIVE}"
echo "${NEXTEST_SHA256}  ${work}/${NEXTEST_ARCHIVE}" | sha256sum -c -
tar -xzf "${work}/${NEXTEST_ARCHIVE}" -C "${work}"
install -d "${CARGO_HOME:-$HOME/.cargo}/bin"
install -m 0755 "${work}/cargo-nextest" "${CARGO_HOME:-$HOME/.cargo}/bin/cargo-nextest"
cargo nextest --version
