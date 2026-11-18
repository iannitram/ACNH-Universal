#!/usr/bin/env bash
# Build the iOS player as ACNH.app from a checkout with no game data (the hand-written stub in tests/ios_stub_gen),
# for the simulator or a device:
#
#   tools/build_ios.sh <iphoneos|iphonesimulator> [<gen dir>]
#
# It fetches and builds static SDL3 release-3.4.18 and MoltenVK v1.3.0 into private/third_party/ios/<sdk>/ (never
# committed), then configures and builds private/ios/build-<sdk>/ACNH.app. Both CI and the Mac's real build use it.
#
# The peer cerroverb/MHGURecomp-iOS (MIT, a fork of morerokk/MHGURecomp) builds the same pieces in its
# .github/workflows/build-ios.yml: a static libMoltenVK.a from the Khronos release, and an SDL3 iOS CMake build.
# This script follows that shape; no code is copied. MoltenVK is Apache-2.0, SDL3 zlib.
#
# Needs: Xcode, CMake, Ninja and the Vulkan headers (brew install vulkan-headers); NX_VULKAN_INCLUDE overrides.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD

usage() { echo "usage: tools/build_ios.sh <iphoneos|iphonesimulator> [<gen dir>]" >&2; exit 2; }
sdk=${1:-}
case "$sdk" in
    iphoneos|iphonesimulator) ;;
    *) usage ;;
esac
gen=${2:-"$root/tests/ios_stub_gen"}
case "$gen" in
    /*) ;;
    *) gen=$root/$gen ;;
esac
[ -d "$gen/main" ] && [ -d "$gen/sdk" ] || { echo "error: $gen/main or $gen/sdk missing" >&2; exit 1; }

third=$root/private/third_party/ios
build=$root/private/ios/build-$sdk
jobs=$(sysctl -n hw.ncpu 2>/dev/null || echo 4)
command -v ninja >/dev/null 2>&1 || { echo "error: ninja not found (brew install ninja)" >&2; exit 1; }

vulkan=${NX_VULKAN_INCLUDE:-}
if [ -z "$vulkan" ] && command -v brew >/dev/null 2>&1; then
    vulkan="$(brew --prefix vulkan-headers 2>/dev/null || true)/include"
fi
if [ ! -f "$vulkan/vulkan/vulkan.h" ]; then
    echo "error: Vulkan headers not found (set NX_VULKAN_INCLUDE or brew install vulkan-headers)" >&2
    exit 1
fi

SDL3_TAG=release-3.4.18
MOLTENVK_TAG=v1.3.0
mkdir -p "$third/downloads" "$third/src" "$third/build" "$third/$sdk"

fetch() { # <url> <file>
    local url=$1 file=$2
    if [ -f "$file" ]; then
        return 0
    fi
    echo "fetching $(basename "$file")"
    curl --fail --silent --show-error --location --retry 3 --output "$file.part" "$url"
    mv "$file.part" "$file"
}

# ---- SDL3 (static, for iOS) -------------------------------------------------------------------------------------
sdl_src=$third/src/SDL-$SDL3_TAG
sdl_build=$third/build/sdl3-$sdk
sdl_prefix=$third/$sdk/sdl3
if [ ! -f "$sdl_prefix/lib/cmake/SDL3/SDL3Config.cmake" ]; then
    fetch "https://github.com/libsdl-org/SDL/archive/refs/tags/$SDL3_TAG.tar.gz" "$third/downloads/SDL-$SDL3_TAG.tar.gz"
    [ -d "$sdl_src" ] || tar -xzf "$third/downloads/SDL-$SDL3_TAG.tar.gz" -C "$third/src"
    cmake -S "$sdl_src" -B "$sdl_build" -G Ninja \
        -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$sdl_prefix" \
        -DCMAKE_SYSTEM_NAME=iOS -DCMAKE_OSX_SYSROOT="$sdk" -DCMAKE_OSX_ARCHITECTURES=arm64 \
        -DCMAKE_OSX_DEPLOYMENT_TARGET=15.0 \
        -DSDL_SHARED=OFF -DSDL_STATIC=ON -DSDL_TESTS=OFF -DSDL_EXAMPLES=OFF
    cmake --build "$sdl_build" --parallel "$jobs"
    cmake --install "$sdl_build"
fi

# ---- MoltenVK (static libMoltenVK.a from KhronosGroup's all-platform release) -----------------------------------
# MoltenVK-ios.tar has only the device slice (ios-arm64); the simulator slice is in MoltenVK-all.tar, so take both
# from that one. The xcframework's static/ directory holds ios-arm64/libMoltenVK.a (device) and
# ios-arm64_x86_64-simulator/libMoltenVK.a (simulator).
mv_prefix=$third/$sdk/moltenvk
mv_archive=$third/downloads/MoltenVK-all-$MOLTENVK_TAG.tar
mv_src=$third/build/moltenvk-all-$MOLTENVK_TAG
fetch "https://github.com/KhronosGroup/MoltenVK/releases/download/$MOLTENVK_TAG/MoltenVK-all.tar" "$mv_archive"
[ -d "$mv_src" ] || { mkdir -p "$mv_src"; tar -xf "$mv_archive" -C "$mv_src"; }
mv_static=$mv_src/MoltenVK/MoltenVK/static/MoltenVK.xcframework
if [ "$sdk" = iphonesimulator ]; then
    mv_lib=$mv_static/ios-arm64_x86_64-simulator/libMoltenVK.a
    [ -f "$mv_lib" ] || mv_lib=$(find "$mv_src" -name libMoltenVK.a -path '*simulator*' -path '*arm64*' -print -quit)
    if [ -z "$mv_lib" ]; then
        mv_lib=$(find "$mv_src" -name libMoltenVK.a -path '*simulator*' -print -quit)
    fi
else
    mv_lib=$mv_static/ios-arm64/libMoltenVK.a
    [ -f "$mv_lib" ] || mv_lib=$(find "$mv_src" -name libMoltenVK.a -path '*/ios-arm64/libMoltenVK.a' -print -quit)
fi
if [ ! -f "$mv_lib" ]; then
    mv_lib=$(find "$mv_src" -name libMoltenVK.a -print -quit)
    echo "warning: no $sdk slice named in the MoltenVK release; using ${mv_lib:-none}" >&2
fi
[ -n "$mv_lib" ] && [ -f "$mv_lib" ] || { echo "error: libMoltenVK.a not found in $mv_archive" >&2; exit 1; }
mkdir -p "$mv_prefix/lib"
cp "$mv_lib" "$mv_prefix/lib/libMoltenVK.a"

# ---- the app ----------------------------------------------------------------------------------------------------
cmake -S "$root" -B "$build" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_SYSTEM_NAME=iOS -DCMAKE_OSX_SYSROOT="$sdk" -DCMAKE_OSX_ARCHITECTURES=arm64 \
    -DCMAKE_OSX_DEPLOYMENT_TARGET=15.0 \
    -DNX_GAME=acnh -DNX_GEN_DIR="$gen" -DNX_LAUNCHER=OFF -DNX_GAME_FROM_FOLDER=OFF \
    -DNX_VULKAN_INCLUDE="$vulkan" -DNX_IOS_MOLTENVK="$mv_prefix/lib/libMoltenVK.a" \
    -DCMAKE_PREFIX_PATH="$sdl_prefix" -DSDL3_DIR="$sdl_prefix/lib/cmake/SDL3"
cmake --build "$build" --target acnh --parallel "$jobs"

app=$build/ACNH.app
plutil -lint "$app/Info.plist"
lipo -archs "$app/ACNH"
echo "built $app"
