#include <nx/runtime.h>
static_assert(nx::kModuleBase == 0x10000000000ull);
static_assert(nx::kGuestShift == 0xC000000000ull);
static_assert(nx::kAddressSpaceBase == 0xC008000000ull);
static_assert(nx::kAddressSpaceSize == 0x7FF8000000ull);
static_assert(nx::kHeapBase == 0x11000000000ull);
static_assert(nx::kAliasBase == 0x11800000000ull);
static_assert(nx::kStackBase == 0x12800000000ull);
static_assert(nx::kKernelBase == 0x12A00000000ull);
static_assert(nx::kThunkBase == 0x12F00000000ull);
static_assert(nx::kHeapMax == 0x180000000ull);
static_assert(nx::kAliasSize == 0x1000000000ull);
static_assert(nx::kStackSize == 0x100000000ull);
static_assert(nx::kKernelSize == 0x100000000ull);
int main() {}
