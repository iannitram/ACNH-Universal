#!/usr/bin/env bash
# Every C++ file of the Apple player compiles for arm64-apple-ios (iphoneos) and arm64-apple-ios-simulator
# (iphonesimulator): syntax only, as in the issue (linking is 8.03b). Run on a Mac with Xcode and Homebrew's
# SDL3 and Vulkan headers: brew install sdl3 vulkan-headers. Without ImGui (private/third_party/imgui), the
# settings screen's stub is checked, as CMake does.
set -euo pipefail
cd "$(dirname "$0")/.."

game=${NX_GAME:-acnh}
sdl3=$(brew --prefix sdl3)
vulkan=$(brew --prefix vulkan-headers)

# brew --prefix prints a path even when the formula is not installed: check the headers themselves.
for header in "$vulkan/include/vulkan/vulkan.h" "$sdl3/include/SDL3/SDL.h"; do
    if [ ! -f "$header" ]; then
        echo "error: $header not found: install the headers with: brew install sdl3 vulkan-headers" >&2
        exit 1
    fi
done

sources=()
while IFS= read -r f; do sources+=("$f"); done < <(
    sed -n '/^set(NX_RUNTIME_SOURCES/,/^)/p' CMakeLists.txt | grep -o 'runtime/src/[^ )]*\.cpp' | sort -u
)
sources+=(runtime/src/platform_mac_wrapper.cpp runtime/src/memguard_mac.cpp runtime/src/cpu_profile.cpp
          runtime/src/app_icon.cpp runtime/src/save_storage.cpp runtime/src/save_profiles.cpp
          runtime/src/mod_manager.cpp)
for f in "games/$game"/*.cpp; do sources+=("$f"); done

imgui=()
if [ -f private/third_party/imgui/imgui.cpp ]; then
    sources+=(runtime/src/ui/ui.cpp runtime/src/ui/settings.cpp runtime/src/ui/amiibo_importer.cpp
              runtime/src/ui/i18n.cpp runtime/src/ui/theme.cpp runtime/src/ui/ui_cjk_font.cpp
              private/third_party/imgui/imgui.cpp private/third_party/imgui/imgui_draw.cpp
              private/third_party/imgui/imgui_tables.cpp private/third_party/imgui/imgui_widgets.cpp
              private/third_party/imgui/backends/imgui_impl_vulkan.cpp)
    imgui=(-Iprivate/third_party/imgui -DIMGUI_IMPL_VULKAN_NO_PROTOTYPES -DNX_HAVE_SDL3)
else
    sources+=(runtime/src/ui/ui_stub.cpp)
fi

common=(-std=c++20 -fsyntax-only -Wno-deprecated-declarations -Wno-\#warnings
        -Iruntime/include -Iruntime/src -I"games/$game" -I"$sdl3/include" -I"$vulkan/include"
        -DNX_NO_VPX -DNX_NO_VORBIS -DNX_VERSION_STRING='"dev"')
if [ "${#imgui[@]}" -gt 0 ]; then common+=("${imgui[@]}"); fi

status=0
for sdk in iphoneos iphonesimulator; do
    if [ "$sdk" = iphoneos ]; then
        target=arm64-apple-ios
        minimum=-miphoneos-version-min=13.0
    else
        target=arm64-apple-ios-simulator
        minimum=-mios-simulator-version-min=13.0
    fi
    sysroot=$(xcrun --sdk "$sdk" --show-sdk-path)
    echo "== $sdk ($target): ${#sources[@]} files"
    for f in "${sources[@]}"; do
        if ! xcrun --sdk "$sdk" clang++ "${common[@]}" --target="$target" "$minimum" -isysroot "$sysroot" "$f"; then
            echo "FAILED: $sdk $f" >&2
            status=1
        fi
    done
done
exit "$status"
