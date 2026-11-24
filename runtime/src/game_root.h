// Where the game's files are read from, without SDL or a bundle. The rule is one pure function, so it is tested on
// every runner (tests/game_root_test.cpp).
//
// The owner's rulings of 2026-11-18 put the prepared game inside each Apple app, read-only, and his saves, settings
// and logs outside it in the data root. NX_GAME_ROOT, set by the Mac app's launcher, wins; else on Apple a game/
// folder beside SDL's base (Contents/Resources/ in the Mac app, the .app folder on iOS) holding analysis/main.bin is
// the game root; else, on iOS, the game the app downloaded at first start (Library/Application Support/game, issue
// #231) when it holds analysis/main.bin; else the data root, as Linux, Windows, Android and a development build read.
#pragma once

#include <string>

namespace nx::plat {

// env_game_root: NX_GAME_ROOT (nullptr when unset).
// apple: macOS or iOS.
// sdl_base: SDL_GetBasePath() with its trailing slash ("" when there is none).
// file_exists: whether a path is a file (the packaged game's analysis/main.bin).
// data_root: the folder the player writes its own files to, and the fallback for the game.
// downloaded_game: where the in-app download puts the game on iOS ("" elsewhere): after the bundle's, before the data root.
template <class FileExists>
inline std::string choose_game_root(const char* env_game_root, bool apple, const std::string& sdl_base,
                                    FileExists file_exists, const std::string& data_root,
                                    const std::string& downloaded_game = "") {
    if (env_game_root && env_game_root[0]) return env_game_root;
    if (apple && !sdl_base.empty()) {
        const std::string game = sdl_base + "game";
        if (file_exists(game + "/analysis/main.bin")) return game;
    }
    if (apple && !downloaded_game.empty() && file_exists(downloaded_game + "/analysis/main.bin")) return downloaded_game;
    return data_root;
}

}  // namespace nx::plat
