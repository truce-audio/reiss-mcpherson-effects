#!/usr/bin/env bash
# Release driver for the macOS half of a reiss-mcpherson-effects
# release.
#
# Tags HEAD as v<workspace-version> (read from the workspace
# Cargo.toml), builds one signed `.pkg` installer per plugin in
# `truce.toml`, and creates / updates the matching GitHub release.
# Run this first; then run `scripts/release-windows.sh` from WSL on
# a Windows machine and `scripts/release-linux.sh` on a Linux box
# to attach the matching `.exe` / `.tar.gz` files.
#
# Requires: gh, cargo, and an authenticated GitHub CLI session. The
# script installs / upgrades cargo-truce from crates.io to a version
# matching the `truce` requirement in the workspace Cargo.toml.
# Notarization runs automatically when a `TRUCE_NOTARY` keychain
# profile is configured (see `xcrun notarytool store-credentials`);
# without it, the build falls back to `--no-notarize` and produces
# installers that are signed but not notarized.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# --- parse versions from Cargo.toml --------------------------------------
#
# Workspace Cargo.toml: the first `version = "..."` line lives under
# `[workspace.package]` and is inherited by every plugin via
# `version.workspace = true`. The `truce` dependency lives under
# `[workspace.dependencies]` as `truce = { version = "X.Y", ... }`.

pkg_version=$(awk -F\" '/^version[[:space:]]*=/ { print $2; exit }' Cargo.toml)
truce_version=$(sed -n 's/^truce[[:space:]]\{1,\}=.*version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -1)

if [[ -z "$pkg_version" || -z "$truce_version" ]]; then
    echo "could not parse workspace version or truce version from Cargo.toml" >&2
    exit 1
fi

release_tag="v$pkg_version"
echo "==> release tag: $release_tag (truce $truce_version)"

# --- preflight -----------------------------------------------------------

branch=$(git rev-parse --abbrev-ref HEAD)
if [[ "$branch" != "main" ]]; then
    echo "not on main (currently $branch) — refuse to release from a side branch" >&2
    exit 1
fi

git fetch --tags origin
if ! git diff --quiet "HEAD" "origin/$branch"; then
    echo "local main diverges from origin/$branch — push or pull first" >&2
    exit 1
fi

if ! command -v gh >/dev/null; then
    echo "gh CLI not installed (https://cli.github.com)" >&2
    exit 1
fi
gh auth status >/dev/null

# --- tag + push ----------------------------------------------------------

if git rev-parse "refs/tags/$release_tag" >/dev/null 2>&1; then
    echo "==> tag $release_tag already exists, skipping git tag"
else
    echo "==> tagging $release_tag"
    git tag -a "$release_tag" -m "$release_tag"
    git push origin "$release_tag"
fi

# --- install cargo-truce -------------------------------------------------

echo "==> installing cargo-truce@^$truce_version (crates.io)"
# `--force` so an already-installed `cargo-truce` (from a previous
# release run or local dev work) is replaced rather than silently
# kept at the old version. `cargo install` is a no-op when the
# binary already exists, regardless of the requested version.
# `^` prefix so a bare major.minor like `0.54` is accepted as a
# SemVer range; `cargo install --version` rejects unqualified
# two-component versions outright.
cargo install cargo-truce --version "^$truce_version" --locked --force

# --- build installers ----------------------------------------------------

mkdir -p target/dist
rm -f target/dist/*.pkg

# `--formats` enumerates the release targets explicitly so AU v2
# gets built even though `au` defaults to AU v3 in cargo-truce.
# AU v3 / AAX stay outside the release pipeline until their
# signing setups are added. With no `-p`, `cargo truce package`
# iterates every plugin in `truce.toml`, so one invocation
# produces a `.pkg` per plugin.
formats="clap,vst3,au2,standalone"
if xcrun notarytool history --keychain-profile "TRUCE_NOTARY" >/dev/null 2>&1; then
    echo "==> packaging with notarization (formats: $formats)"
    cargo truce package --formats "$formats"
else
    echo "==> packaging without notarization (no TRUCE_NOTARY keychain profile; formats: $formats)"
    cargo truce package --no-notarize --formats "$formats"
fi

mapfile -t pkg_paths < <(ls -1 target/dist/*.pkg 2>/dev/null)
if [[ ${#pkg_paths[@]} -eq 0 ]]; then
    echo "no .pkg produced under target/dist/" >&2
    exit 1
fi
echo "==> built ${#pkg_paths[@]} installer(s):"
for p in "${pkg_paths[@]}"; do echo "    $p"; done

# --- create or update release --------------------------------------------

prev_tag=$(git describe --tags --abbrev=0 "${release_tag}^" 2>/dev/null || true)
notes_file=$(mktemp)
trap 'rm -f "$notes_file"' EXIT

{
    echo "# $release_tag"
    echo
    echo "## Changes"
    echo
    if [[ -n "$prev_tag" ]]; then
        git log "$prev_tag..$release_tag" --pretty=format:"- %s"
    else
        git log --pretty=format:"- %s"
    fi
    echo
    echo
    echo "## Installers"
    echo
    echo "- macOS: one \`.pkg\` per plugin"
    echo "- Windows: attached separately via \`scripts/release-windows.sh\` (WSL)"
    echo "- Linux: attached separately via \`scripts/release-linux.sh\`"
} > "$notes_file"

# Optional extras — attach plugin GUI screenshots once (they aren't
# OS-specific) on the macOS run that creates the release.
extras=()
while IFS= read -r f; do extras+=("$f"); done < <(ls -1 screenshots/reiss-mcpherson-*.png 2>/dev/null || true)

if gh release view "$release_tag" >/dev/null 2>&1; then
    echo "==> release $release_tag already exists — uploading .pkg files"
    gh release upload "$release_tag" "${pkg_paths[@]}" "${extras[@]}" --clobber
else
    echo "==> creating release $release_tag"
    gh release create "$release_tag" "${pkg_paths[@]}" "${extras[@]}" \
        --title "$release_tag" \
        --notes-file "$notes_file"
fi

echo
echo "==> macOS release done."
echo "    Next, on a Windows machine (inside WSL):"
echo "      git fetch --tags origin"
echo "      ./scripts/release-windows.sh"
echo "    And on a Linux machine:"
echo "      git fetch --tags origin"
echo "      ./scripts/release-linux.sh"
