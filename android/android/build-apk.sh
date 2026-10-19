#!/usr/bin/env bash
# Builds Resumo for Android (arm64-v8a) into ../dist/Resumo-android-arm64.apk.
# ABI=x86_64 builds ../dist/Resumo-android-x86_64.apk instead (for an emulator).
#
# Needs: Rust with the aarch64-linux-android target, the Android SDK
# (build-tools and a platform) and the NDK. Paths come from:
#   ANDROID_HOME        SDK folder               (default /opt/android/sdk)
#   ANDROID_NDK_HOME    NDK folder               (default: newest in $ANDROID_HOME/ndk)
#   VLC_AAR             libvlc-all .aar file     (default: downloaded from Maven Central)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
cd "$here"

VLC_VERSION="${VLC_VERSION:-3.7.7}"
ANDROID_HOME="${ANDROID_HOME:-/opt/android/sdk}"
ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)}"
BUILD_TOOLS="$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -1)"
PLATFORM_JAR="$(ls -d "$ANDROID_HOME"/platforms/android-*/android.jar | sort -V | tail -1)"
API=24
ABI="${ABI:-arm64-v8a}"
case "$ABI" in
    arm64-v8a) triple=aarch64-linux-android; suffix=arm64 ;;
    x86_64) triple=x86_64-linux-android; suffix=x86_64 ;;
    *) echo "unsupported ABI $ABI" >&2; exit 1 ;;
esac
TRIPLE_ENV="$(echo "$triple" | tr 'a-z-' 'A-Z_')"
work="${CARGO_TARGET_DIR:-$here/target}"
out="$here/../dist/Resumo-android-$suffix.apk"

# --- libVLC for Android ------------------------------------------------------
vlc_dir="$work/libvlc-$VLC_VERSION"
if [ ! -f "$vlc_dir/jni/$ABI/libvlc.so" ]; then
    aar="${VLC_AAR:-$work/libvlc-all-$VLC_VERSION.aar}"
    if [ ! -f "$aar" ]; then
        mkdir -p "$work"
        curl -fL -o "$aar" \
            "https://repo1.maven.org/maven2/org/videolan/android/libvlc-all/$VLC_VERSION/libvlc-all-$VLC_VERSION.aar"
    fi
    mkdir -p "$vlc_dir"
    unzip -qo "$aar" "jni/$ABI/*" -d "$vlc_dir"
fi
vlc_libs="$vlc_dir/jni/$ABI"

# --- Rust library --------------------------------------------------------------
tc="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
export "CARGO_TARGET_${TRIPLE_ENV}_LINKER=$tc/$triple$API-clang"
export "CC_${triple//-/_}=$tc/$triple$API-clang"
export "AR_${triple//-/_}=$tc/llvm-ar"
export VLC_LIB_DIR="$vlc_libs"
export CARGO_TARGET_DIR="$work"
cargo build --release --target "$triple"

# --- APK -----------------------------------------------------------------------
stage="$work/apk"
rm -rf "$stage"
mkdir -p "$stage/lib/$ABI" "$stage/res"
cp "$work/$triple/release/libresumo.so" "$vlc_libs/libvlc.so" "$vlc_libs/libc++_shared.so" "$stage/lib/$ABI/"

# libVLC's Java part: only its AWindow is used, the surface libVLC draws the
# hardware decoder's frames into (see src/surface.rs). The app's own code
# stays in the Rust library.
if [ ! -f "$vlc_dir/classes.dex" ]; then
    aar="${VLC_AAR:-$work/libvlc-all-$VLC_VERSION.aar}"
    unzip -qo "$aar" classes.jar -d "$vlc_dir"
    "$BUILD_TOOLS/d8" --release --min-api "$API" --lib "$PLATFORM_JAR" --output "$vlc_dir" "$vlc_dir/classes.jar"
fi
cp "$vlc_dir/classes.dex" "$stage/"

"$BUILD_TOOLS/aapt2" compile --dir res -o "$stage/res.zip"
"$BUILD_TOOLS/aapt2" link -o "$stage/base.apk" -I "$PLATFORM_JAR" --manifest AndroidManifest.xml "$stage/res.zip"
(cd "$stage" && zip -qr base.apk lib classes.dex)
"$BUILD_TOOLS/zipalign" -f -p 4 "$stage/base.apk" "$stage/aligned.apk"

# A fixed key, so a new APK installs over the previous one.
keystore="$here/resumo.keystore"
if [ ! -f "$keystore" ]; then
    keytool -genkeypair -keystore "$keystore" -storepass resumo -keypass resumo -alias resumo \
        -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Resumo" >/dev/null
fi
mkdir -p "$(dirname "$out")"
"$BUILD_TOOLS/apksigner" sign --ks "$keystore" --ks-pass pass:resumo --key-pass pass:resumo \
    --ks-key-alias resumo --v4-signing-enabled false --out "$out" "$stage/aligned.apk"
"$BUILD_TOOLS/apksigner" verify "$out"
echo "Built $out"
