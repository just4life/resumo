#!/bin/sh
# Builds dist/VPlayer-windows-x64.zip and dist/VPlayer-Setup-<version>.exe (also zipped)
# from Linux. Needs: rustup target x86_64-pc-windows-gnu, mingw-w64, nsis, zip,
# and VLC_LIB_DIR pointing at the x64 folder of the VideoLAN.LibVLC.Windows
# NuGet package (libvlc.dll, libvlccore.dll, libvlc.lib, plugins/).
set -eu
cd "$(dirname "$0")/.."
: "${VLC_LIB_DIR:?set VLC_LIB_DIR to the libVLC x64 folder}"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
TARGET_DIR=${CARGO_TARGET_DIR:-target}

cargo build --release --target x86_64-pc-windows-gnu

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
mkdir "$STAGE/VPlayer"
cp "$TARGET_DIR/x86_64-pc-windows-gnu/release/vplayer.exe" "$STAGE/VPlayer/"
cp "$VLC_LIB_DIR/libvlc.dll" "$VLC_LIB_DIR/libvlccore.dll" "$STAGE/VPlayer/"
cp -r "$VLC_LIB_DIR/plugins" "$STAGE/VPlayer/"

mkdir -p dist
rm -f dist/VPlayer-windows-x64.zip
(cd "$STAGE" && zip -qr9 - VPlayer) > dist/VPlayer-windows-x64.zip
makensis -V2 -DSRC="$STAGE/VPlayer" -DVERSION="$VERSION" \
  -DOUTFILE="$PWD/dist/VPlayer-Setup-$VERSION.exe" installer/vplayer.nsi
# Some browsers and mail services refuse bare .exe downloads.
rm -f "dist/VPlayer-Setup-$VERSION.zip"
(cd dist && zip -q9 "VPlayer-Setup-$VERSION.zip" "VPlayer-Setup-$VERSION.exe")
echo "dist/VPlayer-windows-x64.zip and dist/VPlayer-Setup-$VERSION.exe (.zip) are ready"
