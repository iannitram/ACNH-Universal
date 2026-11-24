// Where the game's files are read from, without SDL or a bundle. The rule is one pure function, so it is tested on
// every runner (tests/game_root_test.cpp).
//
// The owner's rulings of 2026-11-18 put the prepared game inside each Apple app, read-only, and his saves, settings
// and logs outside it in the data root. NX_GAME_ROOT, set by the Mac app's launcher, wins; else on Apple a game/
// folder beside SDL's base (Contents/Resources/ in the Mac app, the .app folder on iOS) holding analysis/main.bin is
// the game root; else the data root, as Linux, Windows, Android and a development build read.
#pragma once

#include <string>

namespace nx::plat {

// env_game_root: NX_GAME_ROOT (nullptr when unset).
// apple: macOS or iOS.
// sdl_base: SDL_GetBasePath() with its trailing slash ("" when there is none).
// file_exists: whether a path is a file (the packaged game's analysis/main.bin).
// data_root: the folder the player writes its own files to, and the fallback for the game.
template <class FileExists>
inline std::string choose_game_root(const char* env_game_root, bool apple, const std::string& sdl_base,
                                    FileExists file_exists, const std::string& data_root) {
    if (env_game_root && env_game_root[0]) return env_game_root;
    if (apple && !sdl_base.empty()) {
        const std::string game = sdl_base + "game";
        if (file_exists(game + "/analysis/main.bin")) return game;
    }
    return data_root;
}

// Android (#192): NX_GAME_ROOT wins; else <files>/game when it holds analysis/main.bin; else "" (never a folder
// outside the app and never the data root, so the app opens on the first-start copy).
template <class FileExists>
inline std::string choose_android_game_root(const char* env_game_root, const std::string& files_dir,
                                            FileExists file_exists) {
    if (env_game_root && env_game_root[0]) return env_game_root;
    if (!files_dir.empty()) {
        std::string game = files_dir;
        if (game.back() != '/') game += '/';
        game += "game";
        if (file_exists(game + "/analysis/main.bin")) return game;
    }
    return {};
}

}  // namespace nx::plat
