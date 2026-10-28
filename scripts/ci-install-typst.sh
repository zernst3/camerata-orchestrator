#!/usr/bin/env bash
# Install a pinned typst release binary on the CI runner.
#
# Several tests render the audit report to PDF through `typst compile`; without the binary
# they either skip their PDF leg or fail, so the PDF path would go unmeasured. The version
# matches the one used for local development, and the archive is checked against a pinned
# SHA-256 before it is unpacked.

set -euo pipefail

TYPST_VERSION="0.15.0"
TYPST_ARCHIVE="typst-x86_64-unknown-linux-musl.tar.xz"
TYPST_SHA256="59b207df01be2dab9f13e80f73d04d7ff8273ffd46b3dd1b9eef5c60f3eeabea"

work="$(mktemp -d)"
curl -fsSL --retry 3 -o "${work}/${TYPST_ARCHIVE}" \
  "https://github.com/typst/typst/releases/download/v${TYPST_VERSION}/${TYPST_ARCHIVE}"
echo "${TYPST_SHA256}  ${work}/${TYPST_ARCHIVE}" | sha256sum -c -
tar -xJf "${work}/${TYPST_ARCHIVE}" -C "${work}"
sudo install -m 0755 "${work}/typst-x86_64-unknown-linux-musl/typst" /usr/local/bin/typst
typst --version
