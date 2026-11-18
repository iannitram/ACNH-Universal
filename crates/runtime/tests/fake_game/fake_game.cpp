// A fake game library for the boot smoke test: the C ABI the player checks when it loads
// lib<game>_game (runtime/src/game_folder.cpp: nx_game_abi, nx_game_longjmp, nx_module_main,
// nx_module_sdk), with modules that hold no code. No game data. Built by tools/boot_smoke.sh.
#include <nx/module.h>

#include <cstdlib>

#define EXPORT extern "C" __attribute__((visibility("default")))

EXPORT const char* nx_game_abi() { return NX_GAME_ABI; }
EXPORT void nx_game_longjmp(uint64_t, uint64_t) { std::abort(); }

EXPORT const nx::ModuleInfo nx_module_main = {"main"};
EXPORT const nx::ModuleInfo nx_module_sdk = {"sdk"};
