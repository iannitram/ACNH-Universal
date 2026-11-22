// Runtime-selected compact guest window for iOS. The largest-gap walk follows the approach
// described by LASRecomp (peiikai/LASRecomp, MIT); the final fixed reservation is atomic.
#include <nx/runtime.h>
#include "compact_layout.h"

#if defined(NX_COMPACT_LAYOUT)
#include <mach/mach.h>
#include <sys/mman.h>
#include <algorithm>
#include <cstdint>

namespace {
constexpr uint64_t kFloor = 0x20000000ull;  // above every offset in the 512 MiB module range
constexpr uint64_t kLimit = 0x8000000000ull;  // 2^39

uint64_t place_window() {
    const uint64_t page = vm_page_size;
    uint64_t rejected[8]{};
    for (unsigned attempt = 0; attempt < 8; ++attempt) {
        uint64_t cursor = kFloor;
        nx::compact::Choice best;
        while (cursor < kLimit) {
            vm_address_t address = cursor;
            vm_size_t size = 0;
            vm_region_basic_info_data_64_t info{};
            mach_msg_type_number_t count = VM_REGION_BASIC_INFO_COUNT_64;
            mach_port_t object = MACH_PORT_NULL;
            const kern_return_t result = vm_region_64(mach_task_self(), &address, &size,
                VM_REGION_BASIC_INFO_64, reinterpret_cast<vm_region_info_t>(&info), &count, &object);
            if (object != MACH_PORT_NULL) mach_port_deallocate(mach_task_self(), object);
            const uint64_t next = result == KERN_SUCCESS ? std::min<uint64_t>(address, kLimit) : kLimit;
            nx::compact::consider_gap(cursor, next, page, nx::kAddressSpaceSize, rejected, attempt, best);
            if (result != KERN_SUCCESS || address >= kLimit) break;
            if (size == 0 || address > UINT64_MAX - size || address + size <= cursor) break;
            cursor = address + size;
        }
        if (!best.address) return 0;
        vm_address_t addr = best.address;
        if (vm_allocate(mach_task_self(), &addr, nx::kAddressSpaceSize, VM_FLAGS_FIXED) != KERN_SUCCESS) {
            rejected[attempt] = best.address;
            continue;
        }
        if (mprotect(reinterpret_cast<void*>(addr), nx::kAddressSpaceSize, PROT_NONE) == 0) return addr;
        vm_deallocate(mach_task_self(), addr, nx::kAddressSpaceSize);
        return 0;
    }
    return 0;
}
}  // namespace

extern "C" uint64_t nx_guest_base() {
    // Function-local initialization is safe even from a generated module's static initializer.
    static const uint64_t base = place_window();
    return base;
}
#endif
