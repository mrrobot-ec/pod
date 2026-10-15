#!/bin/sh
# Plugin build step for pod (spec 3.5): install a checksum-verified release
# binary that matches this checkout, else the latest release, or build from
# source when there is no release at all, then link ~/.local/bin/pod.
#
# POD_INSTALL_BIN_DIR   link directory (default: $HOME/.local/bin)
# POD_INSTALL_LIB_DIR   where a herdr-managed install keeps its binary
#                       (default: ${XDG_DATA_HOME:-$HOME/.local/share}/pod/bin)
# POD_RELEASE_REPO      owner/repo to download from (default: from `git remote`)
# POD_RELEASE_TAG       release tag to install (default: the tag naming HEAD,
#                       else the repository's latest release)
# POD_INSTALL_FROM_SOURCE=1   skip the download and build from source
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
bin_dir=${POD_INSTALL_BIN_DIR:-$HOME/.local/bin}
lib_dir=${POD_INSTALL_LIB_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/pod/bin}
built="$root/target/release/pod"
cd "$root"

say() { printf 'pod install: %s\n' "$*" >&2; }

# Replaces $2 with a copy of $1 by rename, so a running pod is never
# overwritten in place (macOS kills binaries whose pages change under them).
place() {
    cp "$1" "$2.tmp.$$" &&
        chmod 755 "$2.tmp.$$" &&
        mv -f "$2.tmp.$$" "$2"
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    else
        shasum -a 256 "$1" | cut -d ' ' -f 1
    fi
}

target_triple() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os/$arch" in
        Darwin/arm64) echo aarch64-apple-darwin ;;
        Darwin/x86_64) echo x86_64-apple-darwin ;;
        Linux/x86_64) echo x86_64-unknown-linux-gnu ;;
        Linux/aarch64 | Linux/arm64) echo aarch64-unknown-linux-gnu ;;
        *) return 1 ;;
    esac
}

release_repo() {
    if [ -n "${POD_RELEASE_REPO:-}" ]; then
        echo "$POD_RELEASE_REPO"
        return 0
    fi
    url=$(git remote get-url origin 2>/dev/null) || return 1
    case "$url" in
        https://github.com/*) repo=${url#https://github.com/} ;;
        git@github.com:*) repo=${url#git@github.com:} ;;
        *) return 1 ;;
    esac
    echo "${repo%.git}"
}

# The repository's latest published release (not a draft or prerelease),
# from GitHub's API; no jq, so a plain substitution reads tag_name.
latest_release_tag() {
    curl -fsSL -H 'Accept: application/vnd.github+json' \
        "https://api.github.com/repos/$1/releases/latest" 2>/dev/null |
        sed -n 's/^[[:space:]]*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' |
        head -n 1 | grep .
}

# The tag to install: POD_RELEASE_TAG, else the tag naming HEAD (`herdr plugin
# install` fetches with --depth 1 and no tags, so the remote is asked which tag
# points at HEAD), else the latest release, so a commit pushed after a tag
# still installs a prebuilt binary.
release_tag() {
    if [ -n "${POD_RELEASE_TAG:-}" ]; then
        echo "$POD_RELEASE_TAG"
        return 0
    fi
    git describe --exact-match --tags HEAD 2>/dev/null && return 0
    head=$(git rev-parse HEAD 2>/dev/null) || return 1
    tag=$(GIT_TERMINAL_PROMPT=0 git ls-remote --tags origin 2>/dev/null |
        awk -v head="$head" '$1 == head { sub(/^refs\/tags\//, "", $2); sub(/\^\{\}$/, "", $2); print $2; exit }')
    if [ -n "$tag" ]; then
        echo "$tag"
        return 0
    fi
    latest=$(latest_release_tag "$1") || return 1
    say "no release matches this revision; installing the latest release $latest (its binary may be older than this checkout)"
    echo "$latest"
}

# Places the verified binary at $built on success; returns non-zero when no
# release matches this revision. A checksum mismatch aborts the install.
fetch_release() {
    [ "${POD_INSTALL_FROM_SOURCE:-0}" = 1 ] && return 1
    command -v curl >/dev/null 2>&1 || return 1
    command -v tar >/dev/null 2>&1 || return 1
    triple=$(target_triple) || return 1
    repo=$(release_repo) || return 1
    tag=$(release_tag "$repo") || return 1
    asset="pod-$tag-$triple"
    base="https://github.com/$repo/releases/download/$tag"
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    curl -fsSL "$base/$asset.tar.gz" -o "$tmp/$asset.tar.gz" || return 1
    curl -fsSL "$base/$asset.tar.gz.sha256" -o "$tmp/$asset.sha256" || return 1
    want=$(cut -d ' ' -f 1 "$tmp/$asset.sha256")
    got=$(sha256_of "$tmp/$asset.tar.gz")
    if [ "$want" != "$got" ]; then
        say "checksum mismatch for $asset.tar.gz (wanted $want, got $got)"
        exit 1
    fi
    tar -xzf "$tmp/$asset.tar.gz" -C "$tmp" || return 1
    [ -f "$tmp/pod" ] || return 1
    mkdir -p "$root/target/release" || return 1
    place "$tmp/pod" "$built"
}

if fetch_release; then
    say "installed checksum-verified release binary"
else
    say "no release to install; building from source with cargo"
    command -v cargo >/dev/null 2>&1 || {
        say "cargo not found; install Rust (https://rustup.rs) and retry"
        exit 1
    }
    # --target-dir pins the output path even when CARGO_TARGET_DIR is set.
    cargo build --release --target-dir "$root/target"
fi
[ -x "$built" ] || {
    say "no binary at $built"
    exit 1
}

# `herdr plugin install` runs this in a temporary checkout under
# <config>/herdr/plugins/ and renames it afterwards, so a link into it would
# dangle. Managed installs keep a copy in lib_dir; a linked working tree is
# linked directly so rebuilds take effect.
binary=$built
case "$root" in
    */herdr/plugins/*)
        mkdir -p "$lib_dir"
        place "$built" "$lib_dir/pod"
        binary="$lib_dir/pod"
        ;;
esac

mkdir -p "$bin_dir"
ln -sfn "$binary" "$bin_dir/pod"
say "linked $bin_dir/pod -> $binary"
case ":$PATH:" in
    *":$bin_dir:"*) ;;
    *) say "note: $bin_dir is not on PATH" ;;
esac
