#!/bin/sh
# Ground Station installer: https://groundstation.sh/install
#
#   curl -fsSL https://groundstation.sh/install | sh
#
# Installs the `groundstation` CLI and the `gsd` daemon for the current user.
# No root. Nothing outside $HOME. Verified against the release's SHA256SUMS.
#
# Environment:
#   GROUNDSTATION_VERSION          version to install (default: latest)
#   GROUNDSTATION_INSTALL_DIR      where the launch symlinks go (default: ~/.local/bin)
#   GROUNDSTATION_HOME             where releases are unpacked (default: ~/.local/share/groundstation)
#   GROUNDSTATION_NON_INTERACTIVE  set to 1 to never prompt (CI)
#   GROUNDSTATION_RELEASE_BASE     override the download base (testing)

set -eu

REPO="groundstationhq/groundstation"
BASE="${GROUNDSTATION_RELEASE_BASE:-https://github.com/$REPO/releases}"
VERSION="${GROUNDSTATION_VERSION:-latest}"
INSTALL_DIR="${GROUNDSTATION_INSTALL_DIR:-$HOME/.local/bin}"
GS_HOME="${GROUNDSTATION_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/groundstation}"
NON_INTERACTIVE="${GROUNDSTATION_NON_INTERACTIVE:-0}"

say()  { printf '%s\n' "$*"; }
step() { printf '==> %s\n' "$*"; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

# ---------- guards ----------
if [ "$(id -u)" = "0" ] && [ -n "${SUDO_USER:-}" ] && [ "${GROUNDSTATION_ALLOW_SUDO:-0}" != "1" ]; then
  die "don't run this with sudo: it installs into your own home directory (set GROUNDSTATION_ALLOW_SUDO=1 to override)"
fi
for tool in tar mktemp; do command -v "$tool" >/dev/null 2>&1 || die "$tool is required"; done
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --retry 3 --proto '=https,file' --proto-default https -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q -O "$2" "$1"; }
else
  die "curl or wget is required"
fi
if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
elif command -v openssl >/dev/null 2>&1; then
  sha256() { openssl dgst -sha256 "$1" | sed 's/.*= //'; }
else
  die "sha256sum, shasum or openssl is required to verify the download"
fi

# ---------- platform ----------
os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Darwin)
    # An x86_64 shell under Rosetta still wants the native arm64 build.
    if [ "$arch" = "x86_64" ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = "1" ]; then arch=arm64; fi
    case "$arch" in
      arm64|aarch64) target=aarch64-apple-darwin ;;
      x86_64) target=x86_64-apple-darwin ;;
      *) die "unsupported macOS architecture: $arch" ;;
    esac ;;
  Linux)
    # Linux builds are static musl binaries, so glibc vs musl doesn't matter.
    case "$arch" in
      x86_64|amd64) target=x86_64-unknown-linux-musl ;;
      arm64|aarch64) target=aarch64-unknown-linux-musl ;;
      *) die "unsupported Linux architecture: $arch" ;;
    esac ;;
  *) die "unsupported OS: $os (macOS and Linux are supported; on Windows use WSL)" ;;
esac
step "Platform: $target"

# ---------- version ----------
tmp=$(mktemp -d 2>/dev/null || mktemp -d -t groundstation)
trap 'rm -rf "$tmp"' EXIT INT TERM

if [ "$VERSION" = "latest" ]; then
  fetch "$BASE/latest/download/VERSION" "$tmp/VERSION" || die "couldn't resolve the latest version from $BASE"
  VERSION=$(tr -d ' \r\n' < "$tmp/VERSION")
fi
VERSION="${VERSION#v}"
printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$' || die "not a version: '$VERSION'"
step "Version: $VERSION"

name="groundstation-$VERSION-$target"
asset="$name.tar.gz"
release_url="$BASE/download/v$VERSION"

# ---------- already installed? ----------
release_dir="$GS_HOME/releases/$name"
if [ -x "$release_dir/groundstation" ] && [ -x "$release_dir/gsd" ]; then
  step "$VERSION is already unpacked; refreshing links"
else
  # ---------- download + verify ----------
  step "Downloading $asset"
  fetch "$release_url/$asset" "$tmp/$asset" || die "download failed: $release_url/$asset"
  fetch "$release_url/SHA256SUMS" "$tmp/SHA256SUMS" || die "download failed: $release_url/SHA256SUMS"
  expected=$(grep " $asset\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)
  [ -n "$expected" ] || die "$asset is not listed in SHA256SUMS"
  actual=$(sha256 "$tmp/$asset")
  [ "$actual" = "$expected" ] || die "checksum mismatch for $asset
  expected $expected
  actual   $actual"
  step "Checksum verified"

  # ---------- unpack atomically ----------
  mkdir -p "$GS_HOME/releases"
  staging=$(mktemp -d "$GS_HOME/releases/.staging.XXXXXX")
  tar -xzf "$tmp/$asset" -C "$staging"
  [ -x "$staging/$name/groundstation" ] && [ -x "$staging/$name/gsd" ] || die "archive is missing binaries"
  rm -rf "$release_dir"
  mv "$staging/$name" "$release_dir"
  rmdir "$staging" 2>/dev/null || true
fi

# ---------- link ----------
ln -sfn "$release_dir" "$GS_HOME/current"
mkdir -p "$INSTALL_DIR"
for bin in groundstation gsd; do
  ln -sf "$GS_HOME/current/$bin" "$INSTALL_DIR/$bin"
done
step "Installed to $INSTALL_DIR (release in $release_dir)"

# ---------- shadowing ----------
existing=$(command -v groundstation 2>/dev/null || true)
if [ -n "$existing" ] && [ "$existing" != "$INSTALL_DIR/groundstation" ]; then
  say "note: another groundstation is earlier in your PATH: $existing"
  say "      remove it (e.g. cargo uninstall groundstation) or it will shadow this install."
fi

# ---------- PATH ----------
case ":$PATH:" in
  *":$INSTALL_DIR:"*) on_path=1 ;;
  *) on_path=0 ;;
esac
if [ "$on_path" = "0" ]; then
  shell_name=$(basename "${SHELL:-sh}")
  case "$os:$shell_name" in
    Darwin:zsh) rc="$HOME/.zprofile" ;;
    Darwin:bash) rc="$HOME/.bash_profile" ;;
    *:zsh) rc="$HOME/.zshrc" ;;
    *:bash) rc="$HOME/.bashrc" ;;
    *:fish) rc="" ;;
    *) rc="$HOME/.profile" ;;
  esac
  line="export PATH=\"$INSTALL_DIR:\$PATH\""
  if [ -n "$rc" ] && ! grep -Fq "# >>> groundstation >>>" "$rc" 2>/dev/null; then
    printf '\n# >>> groundstation >>>\n%s\n# <<< groundstation <<<\n' "$line" >> "$rc"
    step "Added $INSTALL_DIR to PATH in $rc"
  fi
  if [ "$shell_name" = "fish" ]; then
    say "add to your PATH:  fish_add_path $INSTALL_DIR"
  fi
  say "current terminal:  $line"
fi

# ---------- done ----------
installed=$("$INSTALL_DIR/groundstation" --version 2>/dev/null || echo "groundstation $VERSION")
say ""
say "$installed installed."
say ""
say "next:"
say "  groundstation connect claude-code   # or: codex"
say "  run your agent as usual; then:  groundstation trajectories"
