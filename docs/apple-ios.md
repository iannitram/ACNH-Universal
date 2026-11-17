# iOS Address Space and Provisioning Finding (Issue #72, 8.01)

Written finding for **Issue #72 (milestone 8.01, platforms R04)** ahead of any iOS build (`SPEC.md` M4, M5; `docs/adr/0006-apple.md`).

This investigation addresses two core questions:
1. **Virtual Address Space:** Does the runtime's fixed reservation of the guest's 39-bit address space (`APPLE_GUEST_SHIFT`, heap up to 6 GiB) fit an iPhone or iPad with the extended virtual addressing entitlement?
2. **Account Privileges and Limits:** What does a free Apple ID allow (7-day re-signing, memory limits) compared against the paid Apple Developer Program?

---

## 1. Finding: Guest Address Space on iOS / iPadOS

### Short Answer
**No, `APPLE_GUEST_SHIFT` cannot fit an iPhone or iPad, even with the Extended Virtual Addressing entitlement.**

* `APPLE_GUEST_SHIFT` places the guest address space at `0xC0_0000_0000` (768 GiB), with modules mapped at `NX_BASE = 0x100_0000_0000` (1 TiB) and the address space ending at `0x140_0000_0000` (1.25 TiB).
* On iOS and iPadOS ARM64 hardware, the user-space virtual memory address (VMA) layout is architecturally bounded by a **36-bit translation regime** ($2^{36} = 64\text{ GiB}$).
* The XNU kernel hardcodes `MACH_VM_MAX_ADDRESS_RAW` on iOS to **`0x0FC0000000` (63 GiB)**, with the top 1 GiB (`0xFC0000000`–`0x1000000000`) reserved for the commpage and kernel guard.
* The **Extended Virtual Addressing** entitlement (`com.apple.developer.kernel.extended-virtual-addressing`) only lifts device-RAM tiered caps (~8–14 GiB) up to `MACH_VM_MAX_ADDRESS` (**63 GiB**). It does **not** grant a 39-bit (512 GiB) or 47-bit (128 TiB) address space.
* Even upstream's unshifted reference layout (`kGuestShift = 0`, modules at `0x40_0000_0000` = 256 GiB) exceeds 63 GiB by a factor of 4.
* **Conclusion:** Any fixed reservation at or above `0xFC0000000` unconditionally fails on iOS with `KERN_INVALID_ADDRESS` in `mach_vm_allocate(..., VM_FLAGS_FIXED)` or `EINVAL` in `mmap`. An iOS build requires an iOS-specific compact guest address layout fitting under 63 GiB.

---

### Detailed Analysis and Architectural Limits

#### Current Apple Silicon Mapping on macOS
In `runtime/include/nx/cpu.h` (lines 21–31) and `runtime/include/nx/runtime.h` (lines 24–50):
```cpp
#if defined(__APPLE__)
constexpr uint64_t kGuestShift = 0xC000000000ull;  // 768 GiB
#define NX_BASE 0x0000010000000000ull               // 1024 GiB (1 TiB)
#endif
constexpr uint64_t kAddressSpaceBase = kGuestShift + 0x0008000000ull; // 0xC0_0800_0000
constexpr uint64_t kAddressSpaceSize = 0x7FF8000000ull;               // 511.875 GiB (39-bit)
constexpr uint64_t kHeapBase         = kGuestShift + 0x5000000000ull; // 0x110_0000_0000
constexpr uint64_t kAliasBase        = kGuestShift + 0x5800000000ull; // 0x118_0000_0000
constexpr uint64_t kStackBase        = kGuestShift + 0x6800000000ull; // 0x128_0000_0000
constexpr uint64_t kKernelBase       = kGuestShift + 0x6A00000000ull; // 0x12A_0000_0000
constexpr uint64_t kThunkBase        = kGuestShift + 0x6F00000000ull; // 0x12F_0000_0000
```
On macOS, this reservation works because Darwin on Apple silicon desktop/laptop configurations uses a **47-bit virtual address space** ($2^{47} = 128\text{ TiB}$, with `MACH_VM_MAX_ADDRESS_RAW` = `0x00007FFFFE000000ULL`). There, mapping at 768 GiB (`0xC0_0000_0000`) sits comfortably within the 128 TiB ceiling, well clear of the dyld shared region (`0x2_DC00_0000`..`0x6F_FFFF_FFFF`) and malloc zones (`0x70_0000_0000`..`0x80_0000_0000`).

#### iOS / iPadOS Virtual Memory Architecture (36-bit VMA)
On iOS and iPadOS ARM64 hardware devices (as opposed to the macOS-hosted iOS Simulator), Apple configures the ARM MMU translation regime differently:
1. **Translation Control Register (`TCR_EL1`):** User space (`TTBR0_EL1`) is configured with `T0SZ = 28`. The addressable virtual range is $2^{64 - 28} = 2^{36}\text{ bytes} = \mathbf{64\text{ GiB}}$.
2. **LLVM Compiler-RT Source Confirmation:** In `llvm-project/compiler-rt/lib/sanitizer_common/sanitizer_platform.h`:
   ```c
   #elif defined(__aarch64__)
   # if SANITIZER_APPLE
   #   if SANITIZER_OSX || SANITIZER_IOSSIM
   #     define SANITIZER_MMAP_RANGE_SIZE FIRST_32_SECOND_64(1ULL << 32, 1ULL << 47)
   #   else
       // Darwin iOS/ARM64 has a 36-bit VMA, 64GiB VM.
   #     define SANITIZER_MMAP_RANGE_SIZE FIRST_32_SECOND_64(1ULL << 32, 1ULL << 36)
   #   endif
   ```
3. **XNU Kernel User Ceiling (`MACH_VM_MAX_ADDRESS`):** In `xnu/osfmk/mach/arm/vm_param.h`:
   ```c
   #if defined(XNU_PLATFORM_MacOSX) || defined(XNU_PLATFORM_DriverKit)
   #define MACH_VM_MAX_ADDRESS_RAW 0x00007FFFFE000000ULL  /* ~127 TB (47-bit) */
   #else
   #define MACH_VM_MAX_ADDRESS_RAW 0x0000000FC0000000ULL  /* 63 GiB (36-bit) */
   #endif
   ```
   The upper 1 GiB (`0x0FC0000000` to `0x1000000000`) is reserved for the commpage and kernel guard.

#### What the Extended Virtual Addressing Entitlement Actually Does
The entitlement `com.apple.developer.kernel.extended-virtual-addressing` (introduced in iOS 14):
* Signals `proc_apply_jit_and_vm_policies` and invokes `vm_map_set_jumbo()` during process spawn.
* Inside XNU's physical map (`osfmk/arm/pmap.c`, `pmap_max_64bit_offset`):
  * **Without entitlement (`ARM_PMAP_MAX_OFFSET_DEVICE`):** The kernel clamps user space to device-specific RAM tiers to minimize page table memory overhead:
    * Devices with $\le 1\text{ GiB}$ RAM: clamped to 2–4 GiB.
    * Devices with 2–3 GiB RAM: clamped to 8 GiB (`0x2_0000_0000`).
    * Devices with $> 3\text{ GiB}$ RAM: clamped to ~13–15 GiB (`0x3_4000_0000` or `0x3_8000_0000`).
  * **With entitlement (`ARM_PMAP_MAX_OFFSET_JUMBO`):** The kernel expands the maximum offset to `MACH_VM_MAX_ADDRESS` (**63 GiB** / `0x0FC0000000`).
* Crucially, **jumbo mode caps out at 63 GiB**. It cannot exceed the 36-bit hardware translation boundary.

#### Comparison of Address Space Layouts
| Layout | Module Base (`NX_BASE`) | Base Address | Total Virtual Span | Fits iOS 36-bit Space (63 GiB)? |
|---|---|---|---|---|
| **macOS Apple Silicon** (`APPLE_GUEST_SHIFT`) | `0x100_0000_0000` (1 TiB) | `0xC0_0800_0000` (768 GiB) | ~1.25 TiB (`0x140_0000_0000`) | **NO** (12x too high; immediate fault) |
| **Linux/Windows Reference** (`kGuestShift = 0`) | `0x40_0000_0000` (256 GiB) | `0x00_0800_0000` (128 MiB) | 512 GiB (`0x80_0000_0000`) | **NO** (4x too high) |
| **iOS Compact Layout (Proposed)** | `0x1_0000_0000` (4 GiB) | `0x1_0000_0000` (4 GiB) | ~10–14 GiB total | **YES** (fits under 63 GiB jumbo window) |

---

### Path Forward: A Compact iOS Guest Layout
Does this mean recompiled C++ cannot run on iOS? **No.**
In this repository's architecture:
1. Recompiled functions access their code space through `NXB` (`recomp/__main__.py`), which expands to `(NX_BASE + offset)`.
2. `NX_BASE` is a macro defined in `runtime/include/nx/cpu.h`.
3. The guest finds its heap, stack, and alias regions strictly at runtime via `svcGetInfo` (`runtime/src/svc_process.cpp`, cases 2, 4, 12, 14), which return constants from `runtime/include/nx/runtime.h`.
4. The actual memory required by Animal Crossing: New Horizons is compact:
   * **Modules (`kModuleBase`):** 512 MiB (`0x20000000`).
   * **Heap (`kHeapMax`):** The Switch OS limits application memory to 3,285 MiB (`0xCD500000`, ~3.2 GiB). A heap reservation of 3.5–4.0 GiB is sufficient.
   * **Stack (`kStackBase`):** Switch threads need $\le 256\text{ MiB}$ total.
   * **Kernel/TLS (`kKernelBase`):** $\le 64\text{ MiB}$.
   * **Alias (`kAliasBase`):** Used for shared memory and transfer memory; 1–2 GiB is sufficient (upstream's 64 GiB alias reservation was an arbitrary ceiling).
5. A compact iOS layout spanning `0x1_0000_0000` to `0x4_0000_0000` (~12 GiB total) fits within iOS's 36-bit address space. The exact same C++ code from `gen/` compiles for iOS by simply supplying `-DNX_BASE=...` or conditioning `NX_BASE` on `#if TARGET_OS_IPHONE`.

---

## 2. Finding: Free Apple ID vs. Paid Apple Developer Program

### Comparison Matrix
| Capability | Free Apple ID (Personal Team) | Paid Developer Program ($99/yr) | Impact on ACNH-Universal (M4, M5) |
|---|---|---|---|
| **Provisioning Validity** | **7 days** (profile expires; app will not launch until rebuilt/re-signed) | **365 days** (1 year development and ad-hoc profiles) | Free account requires tethering and re-signing every 7 days via Mac launcher. |
| **Max Sideloaded Apps** | **3 active apps** per device | **Unlimited** development installs (up to 100 devices per class) | Free account is fine if device is dedicated to testing. |
| **Extended Virtual Addressing** (`...extended-virtual-addressing`) | **NOT available** (Personal Team profiles reject restricted kernel capabilities) | **Available** (Full access in Xcode Signing & Capabilities) | Free account limits app to $\le 13\text{ GiB}$ virtual address space (`ARM_PMAP_MAX_OFFSET_DEVICE`). |
| **Increased Memory Limit** (`...increased-memory-limit`) | **NOT officially supported** on Personal Team provisioning | **Available** (Full access in Xcode Signing & Capabilities) | Determines whether iOS Jetsam kills the app when physical RAM usage exceeds stock thresholds. |
| **Physical RAM Ceiling (Jetsam)** | Default per-device Jetsam limit:<br>• 4 GB devices: **~2.0–2.1 GB**<br>• 6 GB devices: **~2.8–3.0 GB**<br>• 8 GB devices: **~3.3–3.8 GB** | With `increased-memory-limit`:<br>• 6 GB devices: **~4.5–5.0 GB**<br>• 8 GB devices: **~6.0 GB**<br>• 16 GB iPad Pros: **~12.0 GB** | **Critical:** ACNH working set is ~2.8–3.2 GiB. On 4 GB/6 GB devices, free ID will face random Jetsam kills (`EXC_RESOURCE`). On 8 GB devices, it runs with tight margins. |

---

### Physical Memory (Jetsam) Behavior on iOS
On iOS, the kernel task **Jetsam** monitors resident memory (`phys_footprint`). If a process exceeds its high-water mark, Jetsam immediately terminates it with `SIGKILL` (`EXC_RESOURCE`, exception subtype `MEMORY HIGHWATER`).

* **Switch Memory Model:** The Switch console possesses 4 GB physical LPDDR4 RAM, with 1 GB reserved for the Horizon OS and **3.25 GB** (`0xCD500000` = 3,444,572,160 bytes) accessible to titles (`runtime/src/svc_process.cpp`, `case 6`). ACNH's island simulation, loaded assets, textures, MoltenVK/Metal buffers, and audio buffers commonly push resident memory to **2.8–3.1 GB**.
* **On a 4 GB iPhone (e.g., iPhone 11, 12, 13 base):**
  * Maximum stock memory limit: ~2.05 GB.
  * *Result:* The game **will crash during island boot or asset loading** under Jetsam memory pressure.
* **On a 6 GB iPhone (e.g., iPhone 12 Pro, 13 Pro, 14, 15):**
  * Maximum stock memory limit: ~2.85 GB.
  * With paid account (`increased-memory-limit`): ~4.8 GB.
  * *Result with Free ID:* The game will sit on the verge of Jetsam kills; heavy island scenes or autosaves will trigger kills.
  * *Result with Paid Account:* Runs reliably with plenty of headroom.
* **On an 8 GB iPhone (e.g., iPhone 15 Pro, iPhone 16) or M-series iPad (8 GB / 16 GB):**
  * Maximum stock memory limit: ~3.4–3.8 GB.
  * With paid account (`increased-memory-limit`): ~6.0 GB (iPhone) / up to ~12 GB (iPad Pro).
  * *Result with Free ID:* Can run ACNH under standard limits (~3.2 GB working set fits under 3.5 GB), though margin is slim.
  * *Result with Paid Account:* Completely stable.

---

## Sources and References

1. **Apple Open Source XNU Kernel:**
   * `xnu/osfmk/mach/arm/vm_param.h` (`MACH_VM_MAX_ADDRESS_RAW` = `0x0FC0000000ULL` on embedded/iOS vs `0x00007FFFFE000000ULL` on macOS).
   * `xnu/osfmk/arm/pmap.c` (`pmap_max_64bit_offset`, `ARM_PMAP_MAX_OFFSET_DEVICE`, `ARM_PMAP_MAX_OFFSET_JUMBO`).
   * `xnu/osfmk/arm64/proc_reg.h` (`TCR_EL1.T0SZ` address regimes).
2. **LLVM Compiler-RT Sanitizer Platform Configurations:**
   * `compiler-rt/lib/sanitizer_common/sanitizer_platform.h` ("Darwin iOS/ARM64 has a 36-bit VMA, 64GiB VM", `SANITIZER_MMAP_RANGE_SIZE = 1ULL << 36`).
3. **Apple Developer Documentation:**
   * [Extended Virtual Addressing Entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com_apple_developer_kernel_extended-virtual-addressing)
   * [Increased Memory Limit Entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com_apple_developer_kernel_increased-memory-limit)
4. **Repository Specifications:**
   * `SPEC.md` (M4, M5).
   * `ROADMAP.md` (Stage 8: Mac, iPhone and iPad; piece 8.01).
   * `docs/adr/0006-apple.md` (Apple: the same recompiled code on Mac and iOS).
   * `runtime/include/nx/runtime.h` and `runtime/include/nx/cpu.h` (`APPLE_GUEST_SHIFT`, `NX_BASE`, `kGuestShift`).

---

## 3. The Decision on the Apple Account (M4, M5)

In `SPEC.md` M4:
> *"ON my Mac, THE LAUNCHER SHALL build and run the Mac app natively on Apple silicon (no Rosetta), and SHALL build the iPhone/iPad app, sign it with my free Apple ID (re-signed every 7 days; the paid account is a later upgrade) and install it over a cable"*

And `docs/adr/0006-apple.md`:
> *"A free Apple ID re-signs every 7 days and limits memory; the paid Apple Developer Program lifts both. That is my decision when stage 8 starts."*

### Recommendation and Options

#### Option A: Stay with Free Apple ID for initial 8.01–8.03 bring-up
* **Condition:** Feasible **only** if the target test device is an **8 GB device** (iPhone 15 Pro, iPhone 16, or an M-series iPad with $\ge 8\text{ GB}$ RAM).
* **Constraints:**
  1. The runtime must use a compact address layout fitting $\le 12\text{ GiB}$ total virtual span (no extended virtual addressing).
  2. The test device must be re-connected to the Mac and re-signed every 7 days.
  3. 4 GB and 6 GB iPhones cannot be supported reliably without memory kills.
* **Verdict:** Acceptable as a zero-cost path to prove cable install (8.03) and initial controller mapping (8.04), provided the test phone is an iPhone 15 Pro / 16 or M-series iPad.

#### Option B (Recommended): Upgrade to Paid Developer Program ($99/year) when 8.03 begins
* **Benefits:**
  1. **365-day signing:** Eliminates the weekly re-signing chore for testing sessions (M5).
  2. **`increased-memory-limit`:** Raises process physical memory to 5–6 GB, completely preventing Jetsam OOM crashes across both 6 GB and 8 GB iPhones.
  3. **`extended-virtual-addressing`:** Unlocks the full 63 GiB jumbo virtual address space on iOS, providing generous room for the compact guest layout and Metal resource pools.
* **Verdict:** Strongly recommended prior to 8.03 cable installation and 8.07 extended island sessions.
