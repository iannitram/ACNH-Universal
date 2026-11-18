// Entirely synthetic module tables and host functions. No game code or game data.
// The loader maps tiny generated images, enters Start -> nnMain, runs both initializers,
// then records, submits and presents a 16x16 red frame through the real NVN/Vulkan runtime.
#include <nx/module.h>

#include <cstdlib>
#include <cstring>
#include <initializer_list>

#define EXPORT extern "C" __attribute__((visibility("default")))

namespace {
constexpr uint64_t kMain = NX_BASE;
constexpr uint64_t kSdk = NX_BASE + 0x10000000;
constexpr uint64_t kTexture = kMain + 0xe00;
constexpr uint64_t kWindow = kMain + 0xe40;
constexpr uint64_t kCommand = kMain + 0xe80;

// Minimal synthetic SDK builders. The runtime queries them via the normal bootstrap path.
#define GETTERS(X) \
    X(nvnTextureBuilderGetTarget, 1) \
    X(nvnTextureBuilderGetFormat, 0x25) \
    X(nvnTextureBuilderGetFlags, 1) \
    X(nvnTextureBuilderGetLevels, 1) \
    X(nvnTextureBuilderGetSamples, 0) \
    X(nvnTextureBuilderGetWidth, 16) \
    X(nvnTextureBuilderGetHeight, 16) \
    X(nvnTextureBuilderGetDepth, 1) \
    X(nvnTextureBuilderGetStride, 0) \
    X(nvnTextureBuilderGetDepthStencilMode, 0) \
    X(nvnTextureBuilderGetMemoryPool, 0) \
    X(nvnTextureBuilderGetMemoryOffset, 0) \
    X(nvnTextureBuilderGetPackagedTextureLayout, 0) \
    X(nvnTextureBuilderGetSwizzle, 0) \
    X(nvnWindowBuilderGetNumTextures, 1) \
    X(nvnWindowBuilderGetTexture, kTexture) \
    X(nvnWindowBuilderGetNumActiveTextures, 1) \
    X(nvnWindowBuilderGetPresentInterval, 1)

#define INDEX(name, value) name##_index,
enum Getter { GETTERS(INDEX) };
#undef INDEX
#define FUNCTION(name, value) void name(nx::CPU& c) { c.x[0] = value; }
GETTERS(FUNCTION)
#undef FUNCTION

void bootstrap(nx::CPU& c) {
    const char* name = reinterpret_cast<const char*>(c.x[0]);
#define LOOKUP(proc, value) if (std::strcmp(name, #proc) == 0) { c.x[0] = kSdk + 0x400 + 4 * proc##_index; return; }
    GETTERS(LOOKUP)
#undef LOOKUP
    c.x[0] = 0;
}

#define IMPORTS(X) \
    X(nvnTextureInitialize) \
    X(nvnWindowInitialize) \
    X(nvnCommandBufferInitialize) \
    X(nvnCommandBufferBeginRecording) \
    X(nvnCommandBufferSetRenderTargets) \
    X(nvnCommandBufferClearColor) \
    X(nvnCommandBufferEndRecording) \
    X(nvnQueueSubmitCommands) \
    X(nvnQueuePresentTexture) \
    X(nvnQueueFinish)
#define INDEX(name) name##_slot,
enum Import { IMPORTS(INDEX) };
#undef INDEX

uint64_t invoke(nx::CPU& c, Import slot, std::initializer_list<uint64_t> args) {
    size_t i = 0;
    for (auto arg : args) c.x[i++] = arg;
    nx::call(c, *reinterpret_cast<const uint64_t*>(kMain + 0xd00 + 8 * slot));
    return c.x[0];
}

void init(nx::CPU&) {}
void nn_main(nx::CPU& c) {
    if (!invoke(c, nvnTextureInitialize_slot, {kTexture, 0})) std::exit(2);
    if (!invoke(c, nvnWindowInitialize_slot, {kWindow, 0})) std::exit(2);
    if (!invoke(c, nvnCommandBufferInitialize_slot, {kCommand, 0})) std::exit(2);
    invoke(c, nvnCommandBufferBeginRecording_slot, {kCommand});
    auto* target = reinterpret_cast<uint64_t*>(kMain + 0xec0);
    *target = kTexture;
    invoke(c, nvnCommandBufferSetRenderTargets_slot, {kCommand, 1, uint64_t(target), 0, 0, 0});
    auto* color = reinterpret_cast<float*>(kMain + 0xed0);
    color[0] = 1.0f; color[1] = 0.0f; color[2] = 0.0f; color[3] = 1.0f;
    invoke(c, nvnCommandBufferClearColor_slot, {kCommand, 0, uint64_t(color), 15});
    auto* handle = reinterpret_cast<uint64_t*>(kMain + 0xee0);
    *handle = invoke(c, nvnCommandBufferEndRecording_slot, {kCommand});
    invoke(c, nvnQueueSubmitCommands_slot, {0, 1, uint64_t(handle)});
    invoke(c, nvnQueuePresentTexture_slot, {0, kWindow, 0});
    invoke(c, nvnQueueFinish_slot, {0});
}

void start(nx::CPU& c) {
    // The relocation resolves to the runtime's nnMain wrapper, which runs the constructors.
    nx::call(c, *reinterpret_cast<const uint64_t*>(kSdk + 0xd00));
}

const nx::FuncEntry main_funcs[] = {{0x100, init}, {0x200, nn_main}};
const nx::FuncEntry sdk_funcs[] = {
    {0x100, init}, {0x200, start}, {0x300, bootstrap},
#define ENTRY(name, value) {0x400 + 4 * name##_index, name},
    GETTERS(ENTRY)
#undef ENTRY
};
const nx::Symbol main_symbols[] = {
    {"nnMain", 0x200, 4, 2, 1, 0},
#define SYMBOL(name) {#name, 0, 0, 2, 1, 1},
    IMPORTS(SYMBOL)
#undef SYMBOL
};
const nx::Reloc main_relocs[] = {
#define RELOC(name) {0xd00 + 8 * name##_slot, nx::R_AARCH64_GLOB_DAT, 1 + name##_slot, 0},
    IMPORTS(RELOC)
#undef RELOC
};
const nx::Symbol sdk_symbols[] = {
    {"_ZN2nn4init5StartEmmPFvvES2_", 0x200, 4, 2, 1, 0},
    {"nvnBootstrapLoader", 0x300, 4, 2, 1, 0},
    {"nnMain", 0, 0, 2, 1, 1},
};
const nx::Reloc sdk_relocs[] = {{0xd00, nx::R_AARCH64_GLOB_DAT, 2, 0}};

template <size_t N> constexpr size_t count(const auto (&)[N]) { return N; }
}  // namespace

EXPORT const char* nx_game_abi() { return NX_GAME_ABI; }
EXPORT void nx_game_longjmp(uint64_t, uint64_t) { std::abort(); }

EXPORT const nx::ModuleInfo nx_module_main = {
    "main", kMain, 0, 0x800, 0x800, 0x400, 0xc00, 0x400, 0x1000, 0,
    main_funcs, count(main_funcs), main_relocs, count(main_relocs), main_symbols, count(main_symbols),
    nullptr, 0, nullptr, 0,
};
EXPORT const nx::ModuleInfo nx_module_sdk = {
    "sdk", kSdk, 0, 0x800, 0x800, 0x400, 0xc00, 0x400, 0x1000, 0,
    sdk_funcs, count(sdk_funcs), sdk_relocs, count(sdk_relocs), sdk_symbols, count(sdk_symbols),
    nullptr, 0, nullptr, 0,
};
