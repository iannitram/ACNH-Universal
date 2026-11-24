// The game root rule (runtime/src/game_root.h): NX_GAME_ROOT wins; on Apple a packaged game/ folder with
// analysis/main.bin wins over the data root; otherwise the data root. No SDL, no bundle, no game file: the temporary
// folders hold empty files. Exit code 0: every case passed.
//   clang++ -std=c++20 -Iruntime/src tests/game_root_test.cpp  (or through CMake -DNX_TESTS=ON, ctest)
#include <cstdio>
#include <filesystem>
#include <string>

#include "game_root.h"

namespace fs = std::filesystem;
using nx::plat::choose_android_game_root;
using nx::plat::choose_game_root;

namespace {

int g_failed = 0;

bool is_file(const std::string& path) {
    std::error_code ec;
    return fs::is_regular_file(fs::path(path), ec);
}

void expect(const std::string& got, const std::string& want, int line) {
    if (got != want) {
        std::printf("FAIL line %d: got \"%s\", expected \"%s\"\n", line, got.c_str(), want.c_str());
        g_failed++;
    }
}

std::string game_root(const char* env, bool apple, const std::string& base, const std::string& data) {
    return choose_game_root(env, apple, base, is_file, data);
}

void touch(const fs::path& file) {
    fs::create_directories(file.parent_path());
    std::FILE* f = std::fopen(file.string().c_str(), "wb");
    if (f) std::fclose(f);
}

}  // namespace

#define EXPECT(got, want) expect((got), (want), __LINE__)

int main() {
    std::error_code ec;
    const fs::path tmp = fs::temp_directory_path(ec) / "acnh-game-root-test";
    fs::remove_all(tmp, ec);
    fs::create_directories(tmp, ec);
    const std::string base = tmp.string() + "/app/";
    const std::string data = tmp.string() + "/data";
    const std::string package = tmp.string() + "/app/game";

    // NX_GAME_ROOT wins over everything, wherever the host is.
    EXPECT(game_root((tmp.string() + "/env-game").c_str(), true, base, data), tmp.string() + "/env-game");
    EXPECT(game_root((tmp.string() + "/env-game").c_str(), false, base, data), tmp.string() + "/env-game");
    touch(fs::path(package) / "analysis" / "main.bin");
    EXPECT(game_root((tmp.string() + "/env-game").c_str(), true, base, data), tmp.string() + "/env-game");

    // On Apple, a base folder whose game/ holds analysis/main.bin gives base + "game".
    EXPECT(game_root(nullptr, true, base, data), package);
    EXPECT(game_root("", true, base, data), package);  // an empty NX_GAME_ROOT is not set

    // A base folder with no game/, or a game/ without analysis/main.bin, gives the data root.
    const std::string empty_base = tmp.string() + "/empty/";
    EXPECT(game_root(nullptr, true, empty_base, data), data);
    fs::remove(fs::path(package) / "analysis" / "main.bin", ec);
    EXPECT(game_root(nullptr, true, base, data), data);
    touch(fs::path(package) / "analysis" / "sdk.bin");
    EXPECT(game_root(nullptr, true, base, data), data);

    // Off Apple with NX_GAME_ROOT unset, it is always the data root.
    touch(fs::path(package) / "analysis" / "main.bin");
    EXPECT(game_root(nullptr, false, base, data), data);
    EXPECT(game_root("", false, base, data), data);

    // Android branch (#192): with NX_GAME_ROOT unset, <files>/game/analysis/main.bin gives <files>/game;
    // without it, it gives "" (never a folder outside the app and never the data root).
    const fs::path android_files = tmp / "android-files";
    const fs::path android_game = android_files / "game";
    const std::string android_data = (android_files / "root").string();
    EXPECT(choose_android_game_root(nullptr, android_files.string(), is_file), "");
    touch(android_game / "analysis" / "main.bin");
    EXPECT(choose_android_game_root(nullptr, android_files.string(), is_file), android_game.string());
    EXPECT(choose_android_game_root("", android_files.string() + "/", is_file), android_game.string());
    EXPECT(choose_android_game_root((tmp.string() + "/env-game").c_str(), android_files.string(), is_file),
           tmp.string() + "/env-game");
    fs::remove(android_game / "analysis" / "main.bin", ec);
    EXPECT(choose_android_game_root(nullptr, android_files.string(), is_file), "");
    // Even if data_root exists, Android never falls back to it.
    touch(fs::path(android_data) / "analysis" / "main.bin");
    EXPECT(choose_android_game_root(nullptr, android_files.string(), is_file), "");

    fs::remove_all(tmp, ec);
    std::printf("game_root_test: %s\n", g_failed ? "FAILED" : "all passed");
    return g_failed ? 1 : 0;
}
