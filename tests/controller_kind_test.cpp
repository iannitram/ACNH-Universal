// The Android controller_kind rule (runtime/src/controller_kind.h), held to the list the launcher and the Rust
// runtime share (tests/data/pad_kinds.tsv): one pad, one set of prompts everywhere. "" when no pad is connected.
//   clang++ -std=c++20 -Iruntime/src tests/controller_kind_test.cpp && ./a.out tests/data/pad_kinds.tsv
//   (or through CMake -DNX_TESTS=ON, ctest)
#include <cstdio>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>

#include "controller_kind.h"

using nx::plat::android_controller_kind;

namespace {

int g_failed = 0;

void expect(const std::string& got, const std::string& want, const std::string& what) {
    if (got != want) {
        std::printf("FAIL %s: got \"%s\", expected \"%s\"\n", what.c_str(), got.c_str(), want.c_str());
        g_failed++;
    }
}

std::vector<std::string> split_tabs(const std::string& line) {
    std::vector<std::string> out;
    std::stringstream ss(line);
    std::string f;
    while (std::getline(ss, f, '\t')) out.push_back(f);
    return out;
}

}  // namespace

int main(int argc, char** argv) {
    if (argc < 2) {
        std::printf("usage: controller_kind_test pad_kinds.tsv\n");
        return 2;
    }
    std::ifstream in(argv[1]);
    std::string line;
    int rows = 0;
    while (std::getline(in, line)) {
        if (line.empty() || line[0] == '#') continue;
        auto f = split_tabs(line);
        if (f.size() != 4) {
            std::printf("FAIL bad row: %s\n", line.c_str());
            g_failed++;
            continue;
        }
        const int vendor = int(std::stoul(f[0], nullptr, 16)), product = int(std::stoul(f[1], nullptr, 16));
        expect(android_controller_kind(true, vendor, product, f[2]), f[3] == "deck" ? "generic" : f[3], line);
        rows++;
    }
    if (rows < 20) {
        std::printf("FAIL only %d rows read\n", rows);
        g_failed++;
    }

    // No connected pad -> "".
    expect(android_controller_kind(false, 0x045E, 0), "", "none xbox");
    expect(android_controller_kind(false, 0x054C, 0), "", "none ps");
    expect(android_controller_kind(false, 0x057E, 0), "", "none switch");
    expect(android_controller_kind(false, 0, 0), "", "none");

    std::printf("controller_kind_test: %s\n", g_failed ? "FAILED" : "all passed");
    return g_failed ? 1 : 0;
}
