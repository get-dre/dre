#!/bin/sh
# Install the dre CLI from a GitHub Release.
#
#   curl -fsSL https://getdre.com/install.sh | sh
#
# DRE_VERSION     the release to install, e.g. v0.0.1-alpha (default: the newest, pre-releases
#                 included)
# DRE_INSTALL_DIR where to put dre (default: ~/.local/bin)
# GITHUB_TOKEN    sent to GitHub's API when finding the newest release, for its higher rate limit
#
# Plugins aren't installed here: `dre deps` (or `dre init`) downloads the ones a project needs.
set -eu
# A failing command in a pipeline fails the pipeline, where the shell can (dash can't; the
# pipelines whose failure matters are checked one by one below).
if (set -o pipefail) 2>/dev/null; then set -o pipefail; fi

repo="get-dre/dre"
dir="${DRE_INSTALL_DIR:-$HOME/.local/bin}"

fail() {
  echo "install.sh: $*" >&2
  exit 1
}

case "$(uname -s)" in
  Darwin) os=macos ;;
  Linux) os=linux ;;
  *) fail "unsupported OS $(uname -s); on Windows, download dre-<version>-windows-<arch>.zip from https://github.com/$repo/releases" ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) fail "unsupported architecture $(uname -m)" ;;
esac

# `api` keeps an error response's body (no -f): GitHub's API says what went wrong there.
if command -v curl >/dev/null 2>&1; then
  api() {
    if [ -n "${GITHUB_TOKEN:-}" ]; then
      curl -sSL -H "Authorization: Bearer $GITHUB_TOKEN" "$1"
    else
      curl -sSL "$1"
    fi
  }
  get_to() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  api() {
    if [ -n "${GITHUB_TOKEN:-}" ]; then
      wget -qO- --header="Authorization: Bearer $GITHUB_TOKEN" "$1"
    else
      wget -qO- "$1"
    fi
  }
  get_to() { wget -qO "$2" "$1"; }
else
  fail "needs curl or wget"
fi

tag="${DRE_VERSION:-}"
if [ -z "$tag" ]; then
  # GitHub's "latest release" skips pre-releases, so take the newest v* tag from the list. The
  # repository also releases its plugins (duckdb-v1.0.0, ...), which can fill whole pages.
  page=1
  while [ -z "$tag" ] && [ "$page" -le 10 ]; do
    list="$(api "https://api.github.com/repos/$repo/releases?per_page=100&page=$page")" ||
      fail "can't list the releases of $repo; set DRE_VERSION (e.g. DRE_VERSION=v0.8.0) to skip the lookup"
    tag="$(printf '%s\n' "$list" | sed -n 's/.*"tag_name": *"\(v[^"]*\)".*/\1/p' | head -n 1)"
    if ! printf '%s\n' "$list" | grep -q '"tag_name"'; then
      # No releases on this page: past the last one, or an error such as the rate limit.
      message="$(printf '%s\n' "$list" | sed -n 's/.*"message": *"\([^"]*\)".*/\1/p' | head -n 1)"
      [ -z "$message" ] ||
        fail "GitHub's API says: $message. A valid GITHUB_TOKEN raises its rate limit; DRE_VERSION (e.g. DRE_VERSION=v0.8.0) skips the lookup"
      break
    fi
    page=$((page + 1))
  done
  [ -n "$tag" ] || fail "can't find a release of $repo"
fi
case "$tag" in v*) ;; *) tag="v$tag" ;; esac
version="${tag#v}"

archive="dre-$version-$os-$arch.tar.gz"
base="https://github.com/$repo/releases/download/$tag"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading dre $tag for $os-$arch"
get_to "$base/$archive" "$tmp/$archive" ||
  fail "can't download $base/$archive; is $tag a release? See https://github.com/$repo/releases"
get_to "$base/SHA256SUMS" "$tmp/SHA256SUMS" || fail "can't download $base/SHA256SUMS"

want="$(awk -v f="$archive" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")"
# No pipeline: a failing checksum tool must not look like a mismatch.
if command -v sha256sum >/dev/null 2>&1; then
  sum="$(sha256sum "$tmp/$archive")" || fail "sha256sum can't read $archive; nothing was installed"
else
  sum="$(shasum -a 256 "$tmp/$archive")" || fail "shasum can't read $archive; nothing was installed"
fi
got="${sum%% *}"
[ -n "$want" ] && [ "$want" = "$got" ] || fail "checksum mismatch for $archive; nothing was installed"

tar -xzf "$tmp/$archive" -C "$tmp" || fail "can't unpack $archive; nothing was installed"
if ! { mkdir -p "$dir" && [ -w "$dir" ]; } 2>/dev/null; then
  fail "can't write to $dir; set DRE_INSTALL_DIR to a folder you own (default: ~/.local/bin), or run the installer with sudo"
fi
mv "$tmp/dre" "$dir/dre" && chmod 755 "$dir/dre" || fail "can't put dre in $dir"
# The receipt tells `dre system update` this is a direct install it may replace.
if [ -f "$tmp/dre-receipt.json" ]; then
  mv "$tmp/dre-receipt.json" "$dir/dre-receipt.json" || fail "can't put dre-receipt.json in $dir"
fi
installed="$("$dir/dre" --version 2>&1)" || fail "installed $dir/dre, but it doesn't run: $installed"
echo "Installed $installed to $dir/dre"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "$dir isn't on your PATH; add it, e.g.: export PATH=\"$dir:\$PATH\"" ;;
esac
