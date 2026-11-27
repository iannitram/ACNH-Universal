// The SDL3 host (platform_sdl_wrapper.cpp), shared by every OS file that has a window: platform_win.cpp,
// platform_linux.cpp and platform_mac.cpp. The window and its Vulkan surface (a CAMetalLayer on Apple), input,
// audio, the settings screen's hooks and the main thread's event loop.
#pragma once

namespace nx::plat {

bool sdl_start();                   // on the main thread, before the game's thread starts: SDL_Init, the main thread
[[noreturn]] void sdl_main_loop();  // the main thread's event loop, for the rest of the process
void* sdl_native_window();          // the window's HWND once it exists, null before (and off Windows); from any thread
void sdl_set_quit_hook(void (*hook)());  // run when the window is closed, before the process exits (the iPad's save sync)

}  // namespace nx::plat
