#!/bin/bash
# Android (arm64-v8a): builds the app (lib<game>.so: the runtime, no game code; Home.java: the screen that asks for the
# game folder) and, from the game set up on this computer, the game's recompiled code as lib<game>_game.so, which
# goes into the game folder on the phone with the data (tools/push_android.sh), never into the APK. Packs the APK,
# signs it with a local debug key and, with --install, installs it on the phone connected by adb.
# The APK also prepares the game folder on the phone (Prepare.java, prepare/prepare_jni.cpp: lib<game>_prepare.so)
# with the tools it carries as native libraries, the only files Android lets an app run: libzig.so (zig for
# aarch64-linux, tools/fetch_zig.py --android) with its lib folder as assets/zig-lib.zip, and libspirv_harness.so (the
# shader tool built NativeAOT for linux-bionic-arm64, tools/build_shader_tool.py: the .NET 10 SDK, on Linux). Made
# once into private/android-tools/ (PREPARE_TOOLS= another folder; a spirv_harness put in its shaders/ is used as it
# is).
#
# usage: tools/build_android.sh [--install] [--jobs N] [--no-game]
#   --no-game: the app only, without the game set up here (what a release carries)
#   --no-phone-prepare: leave out the on-phone Prepare's tools (zig, the shader tool, which builds only on Linux); the
#   app then says on its Prepare screen that this build does not carry them. For an APK whose game comes prepared
#   from a computer (a hub, or the in-app Dropbox download, issue #230).
#   the Vulkan headers: private/third_party/Vulkan-Headers-1.4.357 (tools/setup_acnh.sh; the NDK's are older than
#   what the runtime calls)
#   needs: Android SDK with ndk;28.2.13676358, build-tools;35.0.0, platforms;android-35 (ANDROID_SDK_ROOT, default
#   ~/Library/Android/sdk on macOS, ~/Android/Sdk on Linux) and a JDK 17 (JAVA_HOME; Homebrew's openjdk@17 found by
#   itself)
cd "$(dirname "$0")/.." || exit 1
set -e
GAME=acnh
# macOS: Android Studio's SDK folder; Linux: ~/Android/Sdk.
if [ "$(uname)" = Darwin ]; then HOST=darwin-x86_64; DEF_SDK=$HOME/Library/Android/sdk; else HOST=linux-x86_64; DEF_SDK=$HOME/Android/Sdk; fi
SDK=${ANDROID_SDK_ROOT:-${ANDROID_HOME:-$DEF_SDK}}
NDK=$SDK/ndk/28.2.13676358
BT=$SDK/build-tools/35.0.0
PLATFORM=$SDK/platforms/android-35/android.jar
if [ -z "${JAVA_HOME:-}" ] && [ -d /opt/homebrew/opt/openjdk@17 ]; then export JAVA_HOME=/opt/homebrew/opt/openjdk@17; fi
[ -n "${JAVA_HOME:-}" ] && export PATH=$JAVA_HOME/bin:$PATH
BUILD=private/$GAME/build-android
OUT=private/$GAME/apk
INSTALL=0
# Compile jobs: the cores, as many as the free memory holds (~700 MB each, as tools/setup_acnh.py counts them);
# --jobs or JOBS= to choose.
if [ -n "${JOBS:-}" ]; then :
elif [ -r /proc/meminfo ]; then
  FREE_MB=$(awk '/^MemAvailable:/ { print int($2 / 1024) }' /proc/meminfo)
  CORES=$(nproc)
else  # macOS: half of the memory, as setup_acnh.py
  FREE_MB=$(( $(sysctl -n hw.memsize) / 2097152 ))
  CORES=$(sysctl -n hw.ncpu)
fi
if [ -z "${JOBS:-}" ]; then
  JOBS=$(( (FREE_MB - 1000) / 700 ))
  [ "$JOBS" -gt "$CORES" ] && JOBS=$CORES
  [ "$JOBS" -lt 1 ] && JOBS=1
fi
GAMELIB=1
while [ $# -gt 0 ]; do
  case $1 in
    --install) INSTALL=1 ;;
    --jobs) JOBS=$2; shift ;;
    --no-game) GAMELIB=0 ;;
    --no-phone-prepare) PHONE_PREPARE=0 ;;
    *) echo "unknown option $1"; exit 1 ;;
  esac
  shift
done
[ -d "$NDK" ] || { echo "NDK not found: $NDK"; exit 1; }

# The preparation's tools (prepare/README.md "The tools it runs"), made once.
TOOLS=${PREPARE_TOOLS:-private/android-tools}
if [ "${PHONE_PREPARE:-1}" = 1 ]; then
if [ ! -f "$TOOLS/zig/zig" ] || [ ! -f "$TOOLS/zig/sizes.txt" ]; then
  echo "== zig for the phone (tools/fetch_zig.py aarch64-linux --android)"
  python3 tools/fetch_zig.py aarch64-linux "$TOOLS/zig" --android ||
    { echo "zig could not be downloaded into $TOOLS/zig (no network?): the APK carries it to prepare the game"; exit 1; }
fi
if [ ! -f "$TOOLS/shaders/spirv_harness" ]; then
  echo "== the shader tool for the phone (tools/build_shader_tool.py linux-bionic-arm64)"
  if [ "$(uname)" != Linux ] || ! dotnet --list-sdks 2>/dev/null | grep -q '^10\.'; then
    echo "The shader tool the APK carries (spirv_harness, NativeAOT for linux-bionic-arm64) is built with the .NET 10"
    echo "SDK on Linux: not found here ($(uname); dotnet SDKs: $(dotnet --list-sdks 2>/dev/null | cut -d' ' -f1 | tr '\n' ' '))."
    echo "Install it (https://dotnet.microsoft.com/download/dotnet/10.0) and run this again, or put a spirv_harness"
    echo "built on Linux with tools/build_shader_tool.py linux-bionic-arm64 <dest> into $TOOLS/shaders/."
    exit 1
  fi
  ANDROID_NDK_ROOT=$NDK PATH="$NDK/toolchains/llvm/prebuilt/$HOST/bin:$PATH" \
    python3 tools/build_shader_tool.py linux-bionic-arm64 "$TOOLS/shaders" > "$TOOLS/shaders.log" 2>&1 ||
    { tail -20 "$TOOLS/shaders.log"; echo "the shader tool did not build (log: $TOOLS/shaders.log)"; exit 1; }
fi
# zig's lib folder with its sizes.txt at its top, as the asset the app unpacks (PrepareService.java); zipped again
# when zig changes
if [ ! -f "$TOOLS/zig-lib.zip" ] || [ "$TOOLS/zig/sizes.txt" -nt "$TOOLS/zig-lib.zip" ]; then
  rm -f "$TOOLS/zig-lib.zip.tmp"
  (cd "$TOOLS/zig/lib" && zip -q -r -9 ../../zig-lib.zip.tmp .)
  (cd "$TOOLS/zig" && zip -q -9 ../zig-lib.zip.tmp sizes.txt)
  mv "$TOOLS/zig-lib.zip.tmp" "$TOOLS/zig-lib.zip"
fi
fi  # PHONE_PREPARE
GEN=$PWD/private/$GAME/gen
if [ $GAMELIB = 0 ]; then
  BUILD=private/$GAME/build-android-app
  OUT=private/$GAME/apk-app
  GEN=$PWD/$BUILD/no-gen
elif [ ! -d "$GEN/main" ]; then
  echo "the game is not set up on this computer (python3 tools/setup_acnh.py --game <folder>); --no-game: the app only"
  exit 1
fi
mkdir -p "$(dirname "$OUT")"

# The Rust runtime library (save sync, issue #81: crates/runtime with save-relay's C interface), linked into
# lib<game>.so. Its Rust has no C of its own; the NDK's clang is the linker cargo is told about all the same.
echo "== the Rust runtime library (cargo, aarch64-linux-android)"
command -v cargo >/dev/null 2>&1 || { echo "cargo not found (install Rust: https://rustup.rs)"; exit 1; }
if command -v rustup >/dev/null 2>&1 && ! rustup target list --installed | grep -qx aarch64-linux-android; then
  echo "the Rust target aarch64-linux-android is missing: rustup target add aarch64-linux-android"; exit 1
fi
NDK_BIN=$NDK/toolchains/llvm/prebuilt/$HOST/bin
CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$NDK_BIN/aarch64-linux-android29-clang \
CC_aarch64_linux_android=$NDK_BIN/aarch64-linux-android29-clang AR_aarch64_linux_android=$NDK_BIN/llvm-ar \
  cargo build -p acnh-runtime --release --target aarch64-linux-android -j "$JOBS"
CARGO_OUT=${CARGO_TARGET_DIR:-$PWD/target}
case "$CARGO_OUT" in /*) ;; *) CARGO_OUT=$PWD/$CARGO_OUT ;; esac
RUNTIME_LIB=$CARGO_OUT/aarch64-linux-android/release/libacnh_runtime.a
[ -f "$RUNTIME_LIB" ] || { echo "$RUNTIME_LIB was not built"; exit 1; }

echo "== the Slint launcher library (cargo, aarch64-linux-android, android-launcher)"
CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$NDK_BIN/aarch64-linux-android29-clang \
CC_aarch64_linux_android=$NDK_BIN/aarch64-linux-android29-clang AR_aarch64_linux_android=$NDK_BIN/llvm-ar \
  cargo build -p acnh-app --release --target aarch64-linux-android --features android-launcher --lib -j "$JOBS"
LAUNCHER_LIB=$CARGO_OUT/aarch64-linux-android/release/libacnh_app.so
[ -f "$LAUNCHER_LIB" ] || { echo "$LAUNCHER_LIB was not built"; exit 1; }
RUNTIME_HEADER=$(find "$CARGO_OUT/aarch64-linux-android/release/build" -name acnh_runtime.h -print 2>/dev/null | head -n 1)
[ -n "$RUNTIME_HEADER" ] || { echo "acnh_runtime.h was not generated"; exit 1; }

. tools/build_version.sh
echo "== configure ($BUILD) [version $VNAME ($VCODE)]"
cmake -S . -B "$BUILD" -G Ninja -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" -DANDROID_ABI=arm64-v8a -DANDROID_STL=c++_shared \
  -DANDROID_PLATFORM=android-29 -DNX_GAME=$GAME -DNX_GEN_DIR="$GEN" \
  -DNX_BUILD_VERSION="$VNAME" \
  -DNX_VULKAN_SDK="$PWD/private/third_party/Vulkan-Headers-1.4.357" \
  -DNX_RUNTIME_ARCHIVE="$RUNTIME_LIB" -DNX_RUNTIME_INCLUDE="$(dirname "$RUNTIME_HEADER")" > "$OUT.cmake.log" 2>&1 ||
  { tail -20 "$OUT.cmake.log"; exit 1; }

TARGETS="$GAME nx_prepare_jni"
# (NX_ANDROID_GAME_IN_APP=ON in the build's cache, for comparisons: no separate game library)
[ $GAMELIB = 1 ] && ninja -C "$BUILD" -t targets all | grep -q "^lib${GAME}_game.so:" && TARGETS="$TARGETS ${GAME}_game"
echo "== build $TARGETS with $JOBS jobs (the first time compiles the whole game: ~$(( 300 / JOBS + 5 )) min)"
until mkdir private/build.lock 2>/dev/null; do echo "waiting for another build"; sleep 10; done
trap 'rmdir private/build.lock 2>/dev/null' EXIT
ninja -C "$BUILD" -j "$JOBS" $TARGETS
rmdir private/build.lock; trap - EXIT

echo "== package"
STRIP=$NDK/toolchains/llvm/prebuilt/$HOST/bin/llvm-strip
rm -rf "$OUT"; mkdir -p "$OUT/lib/arm64-v8a"
"$STRIP" --strip-unneeded -o "$OUT/lib/arm64-v8a/lib$GAME.so" "$BUILD/lib$GAME.so"
"$STRIP" --strip-unneeded -o "$OUT/lib/arm64-v8a/lib${GAME}_prepare.so" "$BUILD/lib${GAME}_prepare.so"
"$STRIP" --strip-unneeded -o "$OUT/lib/arm64-v8a/libacnh_app.so" "$LAUNCHER_LIB"
cp "$NDK/toolchains/llvm/prebuilt/$HOST/sysroot/usr/lib/aarch64-linux-android/libc++_shared.so" "$OUT/lib/arm64-v8a/"
# The preparation's tools, run from the app's native library folder (extractNativeLibs in the manifest)
if [ "${PHONE_PREPARE:-1}" = 1 ]; then
  cp "$TOOLS/zig/zig" "$OUT/lib/arm64-v8a/libzig.so"
  cp "$TOOLS/shaders/spirv_harness" "$OUT/lib/arm64-v8a/libspirv_harness.so"
fi
# The game's code, for the game folder (push_android.sh): beside the APK, not in it.
[[ " $TARGETS " == *" ${GAME}_game "* ]] && "$STRIP" --strip-unneeded -o "$OUT/lib${GAME}_game.so" "$BUILD/lib${GAME}_game.so"
# Home.java: javac, then d8 to classes.dex.
mkdir -p "$OUT/classes"
javac -nowarn -Xlint:-options -source 8 -target 8 -bootclasspath "$PLATFORM" -classpath "$BT/core-lambda-stubs.jar" \
  -d "$OUT/classes" $(find android/java -name "*.java")
"$BT/d8" --release --min-api 29 --lib "$PLATFORM" --output "$OUT" $(find "$OUT/classes" -name "*.class")
# libadrenotools' hook libraries, and Turnip when it was built (tools/build_turnip.sh): the Adreno driver it loads.
cp $(find "$BUILD/_deps" -name "lib*hook*.so") "$OUT/lib/arm64-v8a/"
TURNIP=private/third_party/mesa/build-android/src/freedreno/vulkan/libvulkan_freedreno.so
[ -f "$TURNIP" ] && cp "$TURNIP" "$OUT/lib/arm64-v8a/" || echo "(no Turnip: tools/build_turnip.sh; the phone's driver is used)"
# The icons are in git: drawn again only when missing (each Pillow writes other bytes for the same pixels).
[ -f android/res/mipmap-xxxhdpi/ic_launcher.png ] || python3 tools/make_android_icon.py 2>/dev/null || true
"$BT/aapt2" compile --dir android/res -o "$OUT/res.zip"
# VNAME and VCODE come from tools/build_version.sh (sourced before configure so CMake's NX_VERSION_STRING matches).
echo "APK version $VNAME ($VCODE)"
"$BT/aapt2" link -o "$OUT/base.apk" --manifest android/AndroidManifest.xml -I "$PLATFORM" "$OUT/res.zip" \
  --version-name "$VNAME" --version-code "$VCODE" --replace-version
# The pt-BR translation and the SpeedFix files as assets, which the app copies into the game folder (mod_fetch.cpp).
rm -rf "${OUT:?}/assets"
tools/bundle_mods.sh "$OUT/assets"
# Mbed TLS is linked into the game library (Nextendo Online): Apache 2.0 asks for its licence text in the package.
mkdir -p "$OUT/assets/licenses" && cp "$BUILD/_deps/mbedtls-src/LICENSE" "$OUT/assets/licenses/LICENSE-mbedtls.txt"
# save-relay (MIT: its C interface and JNI glue in lib<game>.so, its Java wrapper ported in android/java/org/saverelay)
# and the Rust crates linked into lib<game>.so with the runtime library: the list (crate, version, licence) and each
# crate's own licence texts (tools/rust_licenses.py: offline, no local paths; it fails rather than ship without them).
cp third_party/save-relay/LICENSE "$OUT/assets/licenses/LICENSE-save-relay.txt"
python3 tools/rust_licenses.py "$OUT/assets/licenses" || { echo "the Rust crates' licence notices were not made"; exit 1; }
# Capstone (BSD 3-clause, LLVM's licence for its tables) and tmouh's SpeedFix (CC BY-NC 4.0: its attribution) are built
# into libacnh_prepare.so (the recompiler): their notices
cp third_party/capstone/LICENSE.TXT "$OUT/assets/licenses/LICENSE-capstone.txt"
cp third_party/capstone/LICENSE_LLVM.TXT "$OUT/assets/licenses/LICENSE-capstone-llvm.txt"
cp prepare/recomp/acnh/SPEEDFIX.md "$OUT/assets/licenses/SPEEDFIX.txt"
# zig's licence (MIT; LLVM's and the libraries' own are in its lib folder) and that folder, unpacked on the phone
if [ "${PHONE_PREPARE:-1}" = 1 ]; then
  cp "$TOOLS/zig/LICENSE" "$OUT/assets/licenses/LICENSE-zig.txt"
  cp "$TOOLS/zig-lib.zip" "$OUT/assets/zig-lib.zip"
fi
(cd "$OUT" && zip -q -r base.apk lib classes.dex && zip -q -r -0 base.apk assets)
"$BT/zipalign" -f -P 16 4 "$OUT/base.apk" "$OUT/aligned.apk"
KEY=private/android-debug.keystore
[ -f "$KEY" ] || keytool -genkeypair -keystore "$KEY" -storepass android -keypass android -alias debug \
  -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=ACNH Recomp debug" >/dev/null 2>&1
"$BT/apksigner" sign --ks "$KEY" --ks-pass pass:android --out "$OUT/$GAME.apk" "$OUT/aligned.apk"
ls -la "$OUT/$GAME.apk"

if [ $INSTALL = 1 ]; then
  echo "== install"
  adb install -r "$OUT/$GAME.apk"
fi
