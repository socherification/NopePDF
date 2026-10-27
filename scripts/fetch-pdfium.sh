#!/bin/sh
# Downloads the pinned PDFium build (see pdfium.lock) into vendor/pdfium and
# verifies its checksum. Works on Linux, macOS and Git Bash on Windows.
# Re-downloads only when pdfium.lock changed (tracked by a stamp file).
# Usage: scripts/fetch-pdfium.sh [--force]
set -eu
here=$(cd "$(dirname "$0")" && pwd)
dest="$here/../vendor/pdfium"
stamp="$dest/.nopepdf-stamp"

case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=mac ;;
    MINGW* | MSYS* | CYGWIN*) os=win ;;
    *) echo "Unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
    x86_64 | amd64) arch=x64 ;;
    aarch64 | arm64) arch=arm64 ;;
    *) echo "Unsupported CPU: $(uname -m)" >&2; exit 1 ;;
esac
asset="pdfium-$os-$arch.tgz"
# tr strips CR in case the lock file was checked out with CRLF endings.
version=$(tr -d '' < "$here/pdfium.lock" | awk '$1 == "version" { print $2 }')
expected=$(tr -d '' < "$here/pdfium.lock" | awk -v a="$asset" '$1 == "sha256" && $2 == a { print $3 }')
[ -n "$expected" ] || { echo "No checksum for $asset in pdfium.lock" >&2; exit 1; }
wanted="$version $asset $expected"
if [ -f "$stamp" ] && [ "$(tr -d '' < "$stamp")" = "$wanted" ] && [ "${1:-}" != "--force" ]; then
    echo "PDFium $version already present in $dest (use --force to re-download)"
    exit 0
fi

url="https://github.com/bblanchon/pdfium-binaries/releases/download/$(echo "$version" | sed 's#/#%2F#')/$asset"
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
echo "Downloading $url"
curl -fsSL "$url" -o "$tmp"
if command -v sha256sum >/dev/null; then
    actual=$(sha256sum "$tmp" | cut -d' ' -f1)
else
    actual=$(shasum -a 256 "$tmp" | cut -d' ' -f1)
fi
[ "$actual" = "$expected" ] || { echo "Checksum mismatch for $asset: $actual" >&2; exit 1; }

rm -rf "$dest"
mkdir -p "$dest"
tar -xzf "$tmp" -C "$dest"
echo "$wanted" > "$stamp"
echo "PDFium $version extracted to $dest"
