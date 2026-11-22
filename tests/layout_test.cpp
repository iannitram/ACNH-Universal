#include <nx/runtime.h>
#include <nx/module.h>
#include "compact_layout.h"
#include <cassert>
#include <cstdarg>
#include <initializer_list>
#include <cstdlib>
#include <sys/mman.h>

// A generated module table captures NX_BASE before main, just like the real recompiler output.
const nx::ModuleInfo synthetic_module = {"main", NX_BASE, 0, 0, 0, 0, 0, 0, 0, 0,
    nullptr, 0, nullptr, 0, nullptr, 0, nullptr, 0, nullptr, 0};

namespace nx {
void log(const char*, ...) {}
[[noreturn]] void fatal(const char*, ...) { std::abort(); }
namespace plat {
bool reserve(uint64_t, uint64_t) { return false; } // thunk is optional
bool commit(uint64_t addr, uint64_t size) { return mprotect(reinterpret_cast<void*>(addr), size, PROT_READ | PROT_WRITE) == 0; }
void decommit(uint64_t addr, uint64_t size) { mprotect(reinterpret_cast<void*>(addr), size, PROT_NONE); }
}
}

int main() {
    using namespace nx;
    // Synthetic gaps: the largest usable gap wins, then a retry moves within it.
    const uint64_t rejected[] = {0x10000};
    compact::Choice choice;
    compact::consider_gap(0x10000, 0x30000, 0x1000, 0x10000, nullptr, 0, choice);
    compact::consider_gap(0x40000, 0x48000, 0x1000, 0x10000, nullptr, 0, choice);
    assert(choice.address == 0x10000 && choice.gap_size == 0x20000);
    compact::Choice retry;
    compact::consider_gap(0x10000, 0x30000, 0x1000, 0x10000, rejected, 1, retry);
    assert(retry.address == 0x18000);
    const uint64_t base = NX_BASE;
    assert(base != 0 && base == synthetic_module.base);
    assert(base + kAddressSpaceSize <= 0x8000000000ull);
    assert(kAddressSpaceSize <= 5ull * 1024 * 1024 * 1024);
    assert(kModuleBase == base && kHeapBase == base + kModuleSize);
    assert(kHeapBase + kHeapMax <= kAliasBase);
    assert(kAliasBase + kAliasSize <= kStackBase);
    assert(kStackBase + kStackSize + 0x2000 <= kKernelBase);
    assert(kKernelBase + kKernelSize == base + kAddressSpaceSize);
    // Darwin may split a reservation into several VM entries: sample both ends of each region.
    for (uint64_t address : {base, base + kModuleSize - 0x4000, uint64_t(kHeapBase),
                             uint64_t(kAliasBase), uint64_t(kStackBase), uint64_t(kKernelBase),
                             base + kAddressSpaceSize - 0x4000}) {
        assert(mprotect(reinterpret_cast<void*>(address), 0x4000, PROT_NONE) == 0);
    }
    memory_init();
    assert(vm_query(base).state == kMemInaccessible);
    assert(vm_query(kAliasBase).state == kMemFree);
    assert(vm_query(kStackBase).state == kMemFree);
    assert(vm_query(kKernelBase - 0x2000).state == kMemInaccessible);
    assert(vm_query(kKernelBase).state == kMemInaccessible);
    assert(kernel_alloc(0x4000, 0x4000) == uint64_t(kKernelBase));
    assert(vm_query(base - 1).state == kMemInaccessible);
    assert(vm_query(base + kAddressSpaceSize).state == kMemInaccessible);
}
