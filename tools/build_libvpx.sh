#!/usr/bin/env bash
# libvpx (BSD-3-Clause, WebM Project) cross-built as a static VP8/VP9 decoder for the phone and tablet players, so
# the game's movies play there (runtime/src/hle_movie.cpp: the opening flight to the island is a VP9 movie). Without
# it the Android and iOS builds get NX_NO_VPX and the movies are black (issue #253). The desktop builds take the
# system's libvpx (Linux, macOS) or vcpkg's (Windows, tools/setup_acnh.py) instead.
#
#   tools/build_libvpx.sh android              -> private/third_party/libvpx-android/{include,lib/libvpx.a}
#   tools/build_libvpx.sh iphoneos             -> private/third_party/libvpx-ios/iphoneos/{include,lib/libvpx.a}
#   tools/build_libvpx.sh iphonesimulator      -> private/third_party/libvpx-ios/iphonesimulator/...
#
# CMakeLists.txt links whichever is there (NX_VPX_ANDROID, NX_VPX_IOS). Decoder only: no encoders, tools or tests.
# Needs: git, make, perl; Android: the NDK (ANDROID_SDK_ROOT, as tools/build_android.sh); iOS: Xcode.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD
TAG=v1.16.0
target=${1:-}
case "$target" in android|iphoneos|iphonesimulator) ;; *) sed -n 7,10p "$0"; exit 2 ;; esac

src=$root/private/third_party/libvpx-src-$TAG
if [ ! -d "$src/.git" ]; then
    rm -rf "$src"
    git clone --quiet --depth 1 --branch "$TAG" https://chromium.googlesource.com/webm/libvpx "$src"
fi
jobs=$(sysctl -n hw.ncpu 2>/dev/null || nproc)
common=(--disable-vp8-encoder --disable-vp9-encoder --enable-vp8-decoder --enable-vp9-decoder --disable-examples
        --disable-tools --disable-docs --disable-unit-tests --disable-install-bins --disable-install-srcs
        --enable-static --disable-shared --enable-pic --disable-debug)

if [ "$target" = android ]; then
    SDK=${ANDROID_SDK_ROOT:-${ANDROID_HOME:-$HOME/Library/Android/sdk}}
    NDK=$SDK/ndk/28.2.13676358
    [ -d "$NDK" ] || { echo "NDK not found: $NDK"; exit 1; }
    host=$( [ "$(uname)" = Darwin ] && echo darwin-x86_64 || echo linux-x86_64 )
    bin=$NDK/toolchains/llvm/prebuilt/$host/bin
    out=$root/private/third_party/libvpx-android
    vpxtarget=arm64-android-gcc
    export CC=$bin/aarch64-linux-android29-clang CXX=$bin/aarch64-linux-android29-clang++ AR=$bin/llvm-ar
    export AS=$CC LD=$CC STRIP=$bin/llvm-strip NM=$bin/llvm-nm
    extra=()
else
    sdk=$target
    sysroot=$(xcrun --sdk "$sdk" --show-sdk-path)
    out=$root/private/third_party/libvpx-ios/$target
    if [ "$target" = iphoneos ]; then
        vpxtarget=arm64-darwin-gcc
        minflag=-miphoneos-version-min=15.0
    else
        vpxtarget=generic-gnu  # libvpx has no simulator target: plain C (the Simulator is for checking, not speed)
        minflag=-mios-simulator-version-min=15.0
    fi
    export CC="$(xcrun --sdk "$sdk" -f clang) -isysroot $sysroot $minflag" CXX="$(xcrun --sdk "$sdk" -f clang++) -isysroot $sysroot $minflag"
    export AR=$(xcrun --sdk "$sdk" -f ar) LD="$CC" AS="$CC"
    extra=()
fi

build=$root/private/third_party/libvpx-build-$target
rm -rf "$build" "$out"
mkdir -p "$build"
(
    cd "$build"
    "$src/configure" --target="$vpxtarget" --prefix="$out" "${common[@]}" ${extra[@]+"${extra[@]}"} > configure.log 2>&1 ||
        { tail -30 configure.log; exit 1; }
    make -j"$jobs" > make.log 2>&1 || { tail -30 make.log; exit 1; }
    make install > install.log 2>&1 || { tail -30 install.log; exit 1; }
)
[ -f "$out/lib/libvpx.a" ] || { echo "no $out/lib/libvpx.a"; exit 1; }
cp "$src/LICENSE" "$out/LICENSE"
echo "libvpx $TAG for $target: $out/lib/libvpx.a ($(du -h "$out/lib/libvpx.a" | cut -f1))"
