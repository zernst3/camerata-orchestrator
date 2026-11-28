#!/bin/bash
# install-key-reader: build the stable keychain reader and install it, copying only when it changed.
# (Issue #574. See docs/TECHNICAL.md, "Stable key reader".)
#
# Why: macOS ties a Keychain "Always Allow" to the exact hash of the program that asks. The engine
# changes on every build, the reader (crates/key-reader) only when its own code changes, so the
# approval is given once, to the reader, and survives engine rebuilds.
#
# Usage: scripts/install-key-reader.sh [--debug] [<extra-dir> ...]
#   It builds `camerata-key-reader` (release unless --debug; cargo does nothing when the source is
#   unchanged), then installs the binary to $HOME/.camerata/bin/ (the stable path the engine
#   checks first) and into each <extra-dir> (a folder holding a `camerata` binary, which the
#   engine checks only as a fallback). A destination whose copy has the same SHA-256 is left untouched, so the file's hash
#   never changes without a build change and the one-time approval stays valid.
#   A different build of the reader has a different hash even from the same source: a Rust
#   toolchain bump, a security-framework or lockfile bump, or a profile or RUSTFLAGS change all
#   do it, and each means one more approval. Build it the same way each time.
#   Approve once by running the installed reader yourself (the engine gives up after 5 seconds):
#     ~/.camerata/bin/camerata-key-reader >/dev/null     then enter your password, Always Allow.
# It never reads the keychain, never deletes anything, and overwrites only a destination copy of
# the reader whose hash differs (a new copy is written beside it and moved into place).
set -eu

profile=release; flag="--release"; dests=()
for a in "$@"; do
    case "$a" in
        --debug) profile=debug; flag="" ;;
        -*) echo "unknown option: $a" >&2; exit 2 ;;
        *) dests+=("$a") ;;
    esac
done

top="$(git rev-parse --show-toplevel)"
# shellcheck disable=SC2086
(cd "$top" && cargo build $flag -p camerata-key-reader --locked)
src="$top/target/$profile/camerata-key-reader"
[ -f "$src" ] || { echo "build did not produce $src" >&2; exit 1; }

hash_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
want="$(hash_of "$src")"

dests=("$HOME/.camerata/bin" ${dests[@]+"${dests[@]}"})
for d in "${dests[@]}"; do
    mkdir -p "$d"
    dst="$d/camerata-key-reader"
    if [ -f "$dst" ] && [ "$(hash_of "$dst")" = "$want" ]; then
        echo "unchanged: $dst"
        continue
    fi
    cp "$src" "$dst.new"
    mv -f "$dst.new" "$dst"
    echo "installed: $dst (a NEW build: approve once by running $dst >/dev/null, then Always Allow)"
done
