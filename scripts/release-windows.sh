#!/usr/bin/env bash
# Release driver for the Windows half of a reiss-mcpherson-effects
# release, run from inside WSL. Calls native `cargo.exe` (not the
# WSL Linux cargo) so the produced installers are real Windows
# `.exe` files, then attaches them to the GitHub release created by
# `scripts/release-macos.sh`. Run after the macOS half so the tag
# and release already exist.
#
# Requires: cargo.exe on PATH (install Rust on the Windows side via
# https://rustup.rs), gh, and an authenticated GitHub CLI session.
# The repo must be checked out somewhere `cargo.exe` can read it —
# in practice that means a Windows-side path like `/mnt/c/...`,
# since cargo.exe can't build out of the WSL filesystem reliably.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# --- parse versions from Cargo.toml --------------------------------------

pkg_version=$(awk -F\" '/^version[[:space:]]*=/ { print $2; exit }' Cargo.toml)
truce_version=$(sed -n 's/^truce[[:space:]]\{1,\}=.*version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -1)

if [[ -z "$pkg_version" || -z "$truce_version" ]]; then
    echo "could not parse workspace version or truce version from Cargo.toml" >&2
    exit 1
fi

release_tag="v$pkg_version"
echo "==> release tag: $release_tag (truce $truce_version)"

# --- preflight -----------------------------------------------------------

if ! command -v cargo.exe >/dev/null; then
    echo "cargo.exe not on PATH — install Rust on the Windows side (https://rustup.rs)" >&2
    exit 1
fi
if ! command -v gh >/dev/null; then
    echo "gh CLI not installed (https://cli.github.com)" >&2
    exit 1
fi
gh auth status >/dev/null

git fetch --tags origin

if ! git rev-parse "refs/tags/$release_tag" >/dev/null 2>&1; then
    echo "tag $release_tag does not exist locally or upstream — run scripts/release-macos.sh first" >&2
    exit 1
fi
if ! gh release view "$release_tag" >/dev/null 2>&1; then
    echo "release $release_tag does not exist on GitHub — run scripts/release-macos.sh first" >&2
    exit 1
fi

# --- install cargo-truce (Windows toolchain) -----------------------------

# `cargo.exe install` writes to the Windows-side ~/.cargo/bin and
# `cargo.exe truce package` resolves the subcommand from there, so
# WSL's own cargo / cargo-truce (if any) is irrelevant.
echo "==> installing cargo-truce@^$truce_version via cargo.exe (crates.io)"
# `--force` so any stale `cargo-truce.exe` (prior release, dev
# work) gets replaced rather than silently kept. `cargo install`
# is a no-op when the binary already exists at the same name.
# `^` prefix so a bare major.minor like `0.54` is accepted as a
# SemVer range; `cargo install --version` rejects unqualified
# two-component versions outright.
cargo.exe install cargo-truce --version "^$truce_version" --locked --force

# --- build installers ----------------------------------------------------

mkdir -p target/dist
rm -f target/dist/*.exe

# Windows release targets — `--formats` is set explicitly to match
# what the macOS pipeline ships (no AU on Windows; AAX out until
# its signing path is wired in). With no `-p`, `cargo.exe truce
# package` iterates every plugin in `truce.toml`.
echo "==> packaging via cargo.exe"
cargo.exe truce package --formats clap,vst3,vst2,standalone

# `mapfile` would be cleaner but macOS ships bash 3.2 which doesn't
# have it; the while-read loop is the portable equivalent (WSL
# bash 4+ supports both, but keeping the scripts uniform).
exe_paths=()
while IFS= read -r p; do exe_paths+=("$p"); done < <(ls -1 target/dist/*.exe 2>/dev/null || true)
if [[ ${#exe_paths[@]} -eq 0 ]]; then
    echo "no .exe produced under target/dist/" >&2
    exit 1
fi
echo "==> built ${#exe_paths[@]} installer(s):"
for e in "${exe_paths[@]}"; do echo "    $e"; done

# --- upload --------------------------------------------------------------

echo "==> uploading $(printf '%s ' "${exe_paths[@]##*/}")to release $release_tag"
gh release upload "$release_tag" "${exe_paths[@]}" --clobber

echo
echo "==> Windows release done."
