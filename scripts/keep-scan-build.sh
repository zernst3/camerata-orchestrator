#!/bin/bash
# Keep the build that starts a multi-day inspection, so the same build can finish it.
# (Engine pin, V1 item 39, step D. See camerata-internal docs/plans/2026-11-23_engine-pin-design.md section 3.)
#
# Usage: scripts/keep-scan-build.sh <name> [--dry-run] [--no-build]
#   <name>      short label for the inspection, for example 2026-11-27 or acme
#   --dry-run   print every step, change nothing
#   --no-build  tag and add the worktree, but skip the cargo build
#
# It does exactly this, and nothing else:
#   1. refuses unless the working tree is clean (no uncommitted or untracked files);
#   2. creates the LOCAL tag scan-build/<name> on HEAD;
#   3. adds a worktree at <parent of main checkout>/camerata-scan-<name> on that tag
#      (outside .claude/worktrees, so remove-merged-worktree.sh never sweeps it);
#   4. builds server, app and CLI there, in the worktree's own target folder.
# A worktree, not a copied binary, because the rule library is read from the checkout folder at
# run time: the worktree freezes the rules together with the binary.
# It never pushes a tag or a branch, never deletes or overwrites anything, and refuses if the
# tag or the folder already exists. The full gate on this commit is the operator's job first.
set -eu

name=""; dry=0; build=1
for a in "$@"; do
    case "$a" in
        --dry-run) dry=1 ;;
        --no-build) build=0 ;;
        -*) echo "unknown option: $a" >&2; exit 2 ;;
        *) [ -z "$name" ] && name="$a" || { echo "only one <name> allowed" >&2; exit 2; } ;;
    esac
done
[ -n "$name" ] || { echo "usage: keep-scan-build.sh <name> [--dry-run] [--no-build]" >&2; exit 2; }
case "$name" in
    *[!A-Za-z0-9._-]*|"") echo "name may only use letters, digits, dot, dash, underscore" >&2; exit 2 ;;
esac

top="$(git rev-parse --show-toplevel)"
# The main checkout is the parent of the shared .git folder, even when run from a worktree.
common="$(cd "$top" && cd "$(git rev-parse --git-common-dir)" && pwd)"
main="$(dirname "$common")"
tag="scan-build/$name"
dest="$(dirname "$main")/camerata-scan-$name"

if [ -n "$(git -C "$top" status --porcelain)" ]; then
    echo "Refusing: the working tree has uncommitted or untracked changes. Commit them first; a kept build must be a clean commit." >&2
    exit 1
fi
if git -C "$top" rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    echo "Refusing: tag $tag already exists. Pick another name; nothing is overwritten." >&2
    exit 1
fi
if [ -e "$dest" ]; then
    echo "Refusing: $dest already exists. Pick another name; nothing is overwritten." >&2
    exit 1
fi

commit="$(git -C "$top" rev-parse HEAD)"
echo "Commit: $commit"
echo "Tag (local only, never pushed): $tag"
echo "Worktree: $dest"
if [ "$dry" = 1 ]; then
    echo "Dry run: would tag, add the worktree$([ "$build" = 1 ] && echo ", build server, app and CLI"). Nothing changed."
    exit 0
fi

build_id="${commit:0:12}"
tag_left_behind() {
    echo "The tag $tag was created and is left in place; this script never deletes it." >&2
    echo "To try again under the same name, remove the tag yourself (git tag -d $tag) and any" >&2
    echo "half-made folder $dest, or pick another name." >&2
}
git -C "$top" tag "$tag" "$commit"
if ! git -C "$top" worktree add -q --detach "$dest" "$tag"; then
    echo "Could not add the worktree at $dest." >&2
    tag_left_behind
    exit 1
fi
if [ "$build" = 1 ]; then
    if ! (cd "$dest" && CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-3}" cargo build --release --locked -p camerata-server -p camerata-ui -p camerata); then
        echo "The build failed in $dest." >&2
        tag_left_behind
        exit 1
    fi
fi
echo "Kept build: $build_id (the id a refusal names when another build is opened)."
echo "Start the inspection from this folder: $dest"
echo "To find it again later: git worktree list | grep $build_id"
echo "Do not pull, edit or rebuild there until the inspection is complete."
echo "The tag $tag is local. Push it yourself only if you decide to; this script never does."
