#include <nx/runtime.h>
static_assert(nx::kModuleBase == 0x10000000000ull);
static_assert(nx::kGuestShift == 0xC000000000ull);
static_assert(nx::kHeapMax == 0x180000000ull);
static_assert(nx::kAliasSize == 0x1000000000ull);
static_assert(nx::kStackSize == 0x100000000ull);
static_assert(nx::kKernelSize == 0x100000000ull);
int main() {}
