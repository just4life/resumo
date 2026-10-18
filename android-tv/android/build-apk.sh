#!/usr/bin/env bash
# Builds VPlayer TV for Android TV into ../dist/VPlayer-androidtv.apk, with
# libraries for both 64-bit and 32-bit ARM (many TVs and TV boxes run a
# 32-bit system even on a 64-bit chip). ABIS="x86" builds
# ../dist/VPlayer-androidtv-x86.apk instead, for the Android TV emulator.
#
# Needs: Rust with the aarch64-linux-android and armv7-linux-androideabi
# targets, the Android SDK (build-tools and a platform) and the NDK. Paths come from:
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
ABIS="${ABIS:-arm64-v8a armeabi-v7a}"
work="${CARGO_TARGET_DIR:-$here/target}"
if [ "$ABIS" = "arm64-v8a armeabi-v7a" ]; then
    out="$here/../dist/VPlayer-androidtv.apk"
else
    out="$here/../dist/VPlayer-androidtv-${ABIS// /-}.apk"
fi

# --- libVLC for Android ------------------------------------------------------
vlc_dir="$work/libvlc-$VLC_VERSION"
aar="${VLC_AAR:-$work/libvlc-all-$VLC_VERSION.aar}"
for abi in $ABIS; do
    if [ ! -f "$vlc_dir/jni/$abi/libvlc.so" ]; then
        if [ ! -f "$aar" ]; then
            mkdir -p "$work"
            curl -fL -o "$aar" \
                "https://repo1.maven.org/maven2/org/videolan/android/libvlc-all/$VLC_VERSION/libvlc-all-$VLC_VERSION.aar"
        fi
        mkdir -p "$vlc_dir"
        unzip -qo "$aar" "jni/$abi/*" -d "$vlc_dir"
    fi
done

# --- Rust library, once per ABI ------------------------------------------------
tc="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
export CARGO_TARGET_DIR="$work"
stage="$work/apk-tv"
rm -rf "$stage"
mkdir -p "$stage/res"
for abi in $ABIS; do
    case "$abi" in
        arm64-v8a) triple=aarch64-linux-android; clang=aarch64-linux-android ;;
        armeabi-v7a) triple=armv7-linux-androideabi; clang=armv7a-linux-androideabi ;;
        x86_64) triple=x86_64-linux-android; clang=x86_64-linux-android ;;
        x86) triple=i686-linux-android; clang=i686-linux-android ;;
        *) echo "unsupported ABI $abi" >&2; exit 1 ;;
    esac
    triple_env="$(echo "$triple" | tr 'a-z-' 'A-Z_')"
    vlc_libs="$vlc_dir/jni/$abi"
    env "CARGO_TARGET_${triple_env}_LINKER=$tc/$clang$API-clang" \
        "CC_${triple//-/_}=$tc/$clang$API-clang" \
        "AR_${triple//-/_}=$tc/llvm-ar" \
        "VLC_LIB_DIR=$vlc_libs" \
        cargo build --release --target "$triple"
    mkdir -p "$stage/lib/$abi"
    cp "$work/$triple/release/libvplayer.so" "$vlc_libs/libvlc.so" "$vlc_libs/libc++_shared.so" "$stage/lib/$abi/"
done

# --- libVLC's Java part ---------------------------------------------------------
# Only its AWindow is used: the surface libVLC draws decoded video into (see
# src/surface.rs). The app's own code stays in the Rust library.
if [ ! -f "$vlc_dir/classes.dex" ]; then
    unzip -qo "$aar" classes.jar -d "$vlc_dir"
    "$BUILD_TOOLS/d8" --release --min-api "$API" --lib "$PLATFORM_JAR" --output "$vlc_dir" "$vlc_dir/classes.jar"
fi
cp "$vlc_dir/classes.dex" "$stage/"

# --- APK -----------------------------------------------------------------------
"$BUILD_TOOLS/aapt2" compile --dir res -o "$stage/res.zip"
"$BUILD_TOOLS/aapt2" link -o "$stage/base.apk" -I "$PLATFORM_JAR" --manifest AndroidManifest.xml "$stage/res.zip"
(cd "$stage" && zip -qr base.apk lib classes.dex)
"$BUILD_TOOLS/zipalign" -f -p 4 "$stage/base.apk" "$stage/aligned.apk"

# A fixed key, so a new APK installs over the previous one.
keystore="$here/vplayer.keystore"
if [ ! -f "$keystore" ]; then
    keytool -genkeypair -keystore "$keystore" -storepass vplayer -keypass vplayer -alias vplayer \
        -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=VPlayer" >/dev/null
fi
mkdir -p "$(dirname "$out")"
"$BUILD_TOOLS/apksigner" sign --ks "$keystore" --ks-pass pass:vplayer --key-pass pass:vplayer \
    --ks-key-alias vplayer --v4-signing-enabled false --out "$out" "$stage/aligned.apk"
"$BUILD_TOOLS/apksigner" verify "$out"
echo "Built $out"
