#!/bin/sh
# Builds dist/Resumo-windows-x64.zip and dist/Resumo-Setup-<version>.exe (also zipped)
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
mkdir "$STAGE/Resumo"
cp "$TARGET_DIR/x86_64-pc-windows-gnu/release/resumo.exe" "$STAGE/Resumo/"
cp "$VLC_LIB_DIR/libvlc.dll" "$VLC_LIB_DIR/libvlccore.dll" "$STAGE/Resumo/"
cp -r "$VLC_LIB_DIR/plugins" "$STAGE/Resumo/"

mkdir -p dist
rm -f dist/Resumo-windows-x64.zip
(cd "$STAGE" && zip -qr9 - Resumo) > dist/Resumo-windows-x64.zip
makensis -V2 -DSRC="$STAGE/Resumo" -DVERSION="$VERSION" \
  -DOUTFILE="$PWD/dist/Resumo-Setup-$VERSION.exe" installer/resumo.nsi
# Some browsers and mail services refuse bare .exe downloads.
rm -f "dist/Resumo-Setup-$VERSION.zip"
(cd dist && zip -q9 "Resumo-Setup-$VERSION.zip" "Resumo-Setup-$VERSION.exe")
echo "dist/Resumo-windows-x64.zip and dist/Resumo-Setup-$VERSION.exe (.zip) are ready"
