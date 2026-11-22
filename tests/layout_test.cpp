#include <cassert>
#include <cstdlib>
#include <mach/mach.h>
#include <sys/mman.h>

#include <nx/runtime.h>

#include "compact_layout.h"

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

// A single reservation can span several Mach entries. Check the protection on each one.
static void check_reservation(uint64_t base, uint64_t end) {
    for (uint64_t cursor = base; cursor < end;) {
        vm_address_t address = cursor;
        vm_size_t size = 0;
        vm_region_basic_info_data_64_t info{};
        mach_msg_type_number_t count = VM_REGION_BASIC_INFO_COUNT_64;
        mach_port_t object = MACH_PORT_NULL;
        const auto result = vm_region_64(mach_task_self(), &address, &size, VM_REGION_BASIC_INFO_64,
                                         reinterpret_cast<vm_region_info_t>(&info), &count, &object);
        if (object != MACH_PORT_NULL) mach_port_deallocate(mach_task_self(), object);
        assert(result == KERN_SUCCESS && address <= cursor && address + size > cursor);
        assert(info.protection == VM_PROT_NONE);
        cursor = address + size;
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
    memory_init();
    const uint64_t end = base + kAddressSpaceSize;
    check_reservation(base, end);
    assert(vm_query(kAliasBase - 1).state == kMemInaccessible);
    assert(vm_query(base).state == kMemInaccessible);
    assert(vm_query(kAliasBase).state == kMemFree);
    assert(vm_query(kStackBase).state == kMemFree);
    const auto last_free = vm_query(kStackBase + kStackSize - 1);
    assert(last_free.state == kMemFree && last_free.addr == kAliasBase);
    assert(last_free.addr + last_free.size == kStackBase + kStackSize);
    assert(vm_query(kStackBase + kStackSize).state == kMemInaccessible);
    assert(vm_query(kKernelBase - 0x2000).state == kMemInaccessible);
    assert(vm_query(kKernelBase).state == kMemInaccessible);
    assert(kernel_alloc(0x4000, 0x4000) == uint64_t(kKernelBase));
    assert(vm_query(base - 1).state == kMemInaccessible);
    assert(vm_query(end - 1).state == kMemInaccessible);
    assert(vm_query(end).state == kMemInaccessible);
    const auto high = vm_query(UINT64_MAX);
    assert(high.state == kMemInaccessible && high.addr == end && UINT64_MAX - high.addr < high.size);
}
