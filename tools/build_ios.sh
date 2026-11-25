#!/usr/bin/env bash
# Build the iOS player as ACNH.app from a checkout with no game data (the hand-written stub in tests/ios_stub_gen),
# for the simulator or a device:
#
#   tools/build_ios.sh <iphoneos|iphonesimulator> [<gen dir> [<build dir>]]
#
# It fetches and builds static SDL3 release-3.4.18 and MoltenVK v1.3.0 into private/third_party/ios/<sdk>/ (never
# committed), then configures and builds <build dir>/ACNH.app (default private/ios/build-<sdk>/; the launcher's prepare gives a
# folder under the prepared folder, so a build that holds the recompiled code is never written inside a git work tree). Both CI and the Mac's real build use it.
#
# The peer cerroverb/MHGURecomp-iOS (MIT, a fork of morerokk/MHGURecomp) builds the same pieces in its
# .github/workflows/build-ios.yml: a static libMoltenVK.a from the Khronos release, and an SDL3 iOS CMake build.
# This script follows that shape; no code is copied. MoltenVK is Apache-2.0, SDL3 zlib.
#
# It builds libvpx for the SDK too (tools/build_libvpx.sh, into private/third_party/libvpx-ios/<sdk>) when it is not
# there, so the game's movies play, and refuses a configure that dropped it.
#
# It also builds the Rust runtime library (save-relay in the player, issue #172): cargo build -p acnh-runtime --release
# --target aarch64-apple-ios (aarch64-apple-ios-sim for the simulator), and fails unless the SDL3 it built has the
# GameController joystick driver (SDL_JOYSTICK_MFI), which the pads need.
#
# Needs: Xcode, CMake, Ninja, Rust with the iOS target (rustup target add aarch64-apple-ios) and the Vulkan headers
# (brew install vulkan-headers); NX_VULKAN_INCLUDE overrides.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD

usage() { echo "usage: tools/build_ios.sh <iphoneos|iphonesimulator> [<gen dir> [<build dir>]]" >&2; exit 2; }
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
build=${3:-"$root/private/ios/build-$sdk"}
case "$build" in
    /*) ;;
    *) build=$root/$build ;;
esac
# Compile jobs: the cores, as many as half the memory holds (~700 MB each, as tools/build_android.sh counts
# them); JOBS= to choose. One job per core ran an 8 GB Mac out of memory on the recompiled game.
if [ -n "${JOBS:-}" ]; then
    jobs=$JOBS
else
    cores=$(sysctl -n hw.ncpu 2>/dev/null || echo 4)
    mem_mb=$(( $(sysctl -n hw.memsize 2>/dev/null || echo 8589934592) / 2097152 ))
    jobs=$(( (mem_mb - 1000) / 700 ))
    [ "$jobs" -gt "$cores" ] && jobs=$cores
    [ "$jobs" -lt 1 ] && jobs=1
fi
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

# Controllers on the iPad come through SDL3's GameController driver: refuse an SDL3 built without it.
sdl_config=$(find "$sdl_build" "$sdl_prefix" -name SDL_build_config.h -print 2>/dev/null | head -n 1)
if [ -z "$sdl_config" ] || ! grep -Eq '^[[:space:]]*#[[:space:]]*define[[:space:]]+SDL_JOYSTICK_MFI[[:space:]]+1' "$sdl_config"; then
    echo "error: the iOS SDL3 build does not define SDL_JOYSTICK_MFI (${sdl_config:-no SDL_build_config.h found}); controllers would not work" >&2
    exit 1
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

# ---- the Rust runtime library (save-relay in the player) --------------------------------------------------------
command -v cargo >/dev/null 2>&1 || { echo "error: cargo not found (install Rust: https://rustup.rs)" >&2; exit 1; }
if [ "$sdk" = iphonesimulator ]; then rust_target=aarch64-apple-ios-sim; else rust_target=aarch64-apple-ios; fi
if command -v rustup >/dev/null 2>&1 && ! rustup target list --installed | grep -qx "$rust_target"; then
    echo "error: the Rust target $rust_target is missing: rustup target add $rust_target" >&2
    exit 1
fi
IPHONEOS_DEPLOYMENT_TARGET=15.0 cargo build -p acnh-runtime --release --target "$rust_target"
cargo_target=${CARGO_TARGET_DIR:-$root/target}
case "$cargo_target" in /*) ;; *) cargo_target=$root/$cargo_target ;; esac
runtime_lib=$cargo_target/$rust_target/release/libacnh_runtime.a
[ -f "$runtime_lib" ] || { echo "error: $runtime_lib was not built" >&2; exit 1; }
runtime_header=$(find "$cargo_target/$rust_target/release/build" -name acnh_runtime.h -print 2>/dev/null | head -n 1)

# ---- libvpx (the movies: tools/build_libvpx.sh; without it the game's movies are black frames, issue #253) ----------
# NX_VPX_DIR (Prepare sets it beside the prepared folder, which survives; the source copy it runs in is made anew each
# time) holds libvpx's clone, build and result; it needs git, make, perl and the network the first time only.
export NX_VPX_DIR=${NX_VPX_DIR:-$root/private/third_party}
vpx_root=$NX_VPX_DIR/libvpx-ios
if [ ! -f "$vpx_root/$sdk/lib/libvpx.a" ]; then
    "$root/tools/build_libvpx.sh" "$sdk" ||
        { echo "error: libvpx could not be built (needs git, make, perl and the network, once): the movies would be black" >&2; exit 1; }
fi
[ -f "$vpx_root/$sdk/lib/libvpx.a" ] && [ -f "$vpx_root/$sdk/include/vpx/vpx_decoder.h" ] ||
    { echo "error: libvpx for $sdk is missing under $vpx_root/$sdk (tools/build_libvpx.sh $sdk)" >&2; exit 1; }

# ---- the app ----------------------------------------------------------------------------------------------------
. "$root/tools/build_version.sh"
ios_short_version=${BASE_VER%%-*}
ios_short_version=${ios_short_version%%+*}
echo "iOS version $VNAME (CFBundleShortVersionString=$ios_short_version, CFBundleVersion=$VCODE)"
# The game build the recompiled code is for (Prepare's version.ini beside gen/, else a game.ini there): the small .ipa
# refuses a downloaded game of another build, since the code is linked in. NX_APPGAME_BUILDS overrides; none known
# accepts any.
appgame_builds=${NX_APPGAME_BUILDS:-}
if [ -z "$appgame_builds" ]; then
    for ini in "$gen/../version.ini" "$gen/../game.ini"; do
        [ -f "$ini" ] || continue
        appgame_builds=$(sed -n 's/^[[:space:]]*build_id[[:space:]]*=[[:space:]]*\([0-9A-Fa-f]\{32\}\)[[:space:]]*$/\1/p' "$ini" \
            | head -n 1 | tr 'a-f' 'A-F')
        [ -n "$appgame_builds" ] && break
    done
fi
cmake -S "$root" -B "$build" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_SYSTEM_NAME=iOS -DCMAKE_OSX_SYSROOT="$sdk" -DCMAKE_OSX_ARCHITECTURES=arm64 \
    -DCMAKE_OSX_DEPLOYMENT_TARGET=15.0 \
    -DNX_GAME=acnh -DNX_GEN_DIR="$gen" -DNX_GAME_FROM_FOLDER=OFF \
    -DNX_BUILD_VERSION="$VNAME" \
    -DNX_APPGAME_BUILDS="$appgame_builds" \
    -DNX_IOS_SHORT_VERSION="$ios_short_version" -DNX_IOS_BUNDLE_VERSION="$VCODE" \
    -DNX_VULKAN_INCLUDE="$vulkan" -DNX_VPX_IOS="$vpx_root" -DNX_IOS_MOLTENVK="$mv_prefix/lib/libMoltenVK.a" \
    -DNX_RUNTIME_ARCHIVE="$runtime_lib" ${runtime_header:+-DNX_RUNTIME_INCLUDE="$(dirname "$runtime_header")"} \
    -DCMAKE_PREFIX_PATH="$sdl_prefix" -DSDL3_DIR="$sdl_prefix/lib/cmake/SDL3"
grep -q 'NX_NO_VPX' "$build/build.ninja" && { echo "error: the app is configured without libvpx (NX_NO_VPX): the movies would be black" >&2; exit 1; }
cmake --build "$build" --target acnh --parallel "$jobs"

app=$build/ACNH.app
plutil -lint "$app/Info.plist"
lipo -archs "$app/ACNH"
echo "built $app"
