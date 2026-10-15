#!/usr/bin/env bash
# Build a self-contained Windows x64 server zip: server.exe, the dashboard, a
# settings file and instructions. Cross-compiles with MinGW, so it runs on the
# workstation (macOS or Linux) rather than needing a Windows build machine.
#
#   brew install mingw-w64                       # or apt install mingw-w64
#   rustup target add x86_64-pc-windows-gnu
#   ./scripts/package_windows.sh                 # -> dist/tv-controller-server-<version>-windows-x64.zip
#
# VERSION defaults to the server crate's version.
set -euo pipefail

cd "$(dirname "$0")/.."

TARGET=x86_64-pc-windows-gnu
VERSION="${VERSION:-v$(sed -n 's/^version = "\(.*\)"/\1/p' server/Cargo.toml | head -1)}"
NAME="tv-controller-server-${VERSION}-windows-x64"
OUT_DIR="dist"
STAGE="$OUT_DIR/$NAME"

command -v x86_64-w64-mingw32-gcc >/dev/null \
  || { echo "x86_64-w64-mingw32-gcc not found: install mingw-w64" >&2; exit 1; }

echo "== Building server.exe"
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  cargo build --release -p server --target "$TARGET"

echo "== Building dashboard"
(cd dashboard && npm ci --silent && npm run build)

echo "== Staging $STAGE"
rm -rf "$STAGE" "$OUT_DIR/$NAME.zip"
mkdir -p "$STAGE/dashboard" "$STAGE/videos"
cp "target/$TARGET/release/server.exe" "$STAGE/"
cp -R server/dashboard/dist "$STAGE/dashboard/dist"
# CRLF, since these are opened in Notepad and run by cmd.exe — batch files with
# bare LF misparse labels and long lines.
for f in env:.env start.bat:start.bat README.txt:README.txt; do
  perl -pe 's/\r?\n/\r\n/' "packaging/windows/${f%%:*}" > "$STAGE/${f#*:}"
done
# Zip drops empty directories on some extractors; keep videos\ present.
printf 'Put videos and images here. No subfolders.\r\n' > "$STAGE/videos/PUT_MEDIA_HERE.txt"

echo "== Zipping"
(cd "$OUT_DIR" && zip -qr "$NAME.zip" "$NAME")
echo "$OUT_DIR/$NAME.zip"
