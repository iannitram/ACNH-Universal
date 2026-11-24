// Aspect, projection, safe-edge, and letterbox math unit tests (runtime/src/nvn/nvn_aspect.cpp).
// Built by CMake with -DNX_TESTS=ON (ctest -R aspect). Exit code 0: all cases passed.
#include "nvn_aspect.h"

#include <cmath>
#include <cstdio>

namespace {

int g_failures = 0;

void check_true(bool cond, const char* msg, int line) {
    if (!cond) {
        std::fprintf(stderr, "FAIL line %d: %s\n", line, msg);
        g_failures++;
    }
}

void check_near(double a, double b, double eps, const char* msg, int line) {
    if (!(std::fabs(a - b) <= eps)) {
        std::fprintf(stderr, "FAIL line %d: %s (got %.6f, expected %.6f, eps %.6f)\n", line, msg, a, b, eps);
        g_failures++;
    }
}

#define CHECK(cond) check_true((cond), #cond, __LINE__)
#define CHECK_NEAR(a, b, eps) check_near(double(a), double(b), double(eps), #a " ~= " #b, __LINE__)

double horizontal_fov(float fov_y, float aspect) {
    return 2.0 * std::atan(std::tan(double(fov_y) * 0.5) * double(aspect));
}

void test_target_resolutions() {
    struct DeviceCase {
        const char* name;
        uint32_t w;
        uint32_t h;
        double expected_k;
        bool is_wide;
        bool is_tall;
    };
    const DeviceCase cases[] = {
        {"Pixel Fold unfolded (6:5)", 2208, 1840, 173.0 / 256.0, false, true},
        {"Pixel Fold folded (17.4:9)", 2092, 1080, 279.0 / 256.0, true, false},
        {"iPad mini (3:2)", 2266, 1488, 219.0 / 256.0, false, true},
        {"1080p projector (16:9)", 1920, 1080, 1.0, false, false},
        {"Flow Z13 (16:10)", 2560, 1600, 230.0 / 256.0, false, true},
        {"Ultrawide (2560x1080)", 2560, 1080, 341.0 / 256.0, true, false},
        {"Super-ultrawide (32:9)", 3840, 1080, 2.0, true, false},
        {"Pixel Fold upright (5:6)", 1840, 2208, 120.0 / 256.0, false, true},
    };

    const float base_fov_y = 0.384f;  // ~22 deg world camera
    const double base_fov_x = horizontal_fov(base_fov_y, float(nx::nvn::kBaseAspect169));

    for (const auto& c : cases) {
        const float k = nx::nvn::parse_aspect_factor("auto", c.w, c.h);
        CHECK_NEAR(k, c.expected_k, 1e-5);

        const nx::nvn::AspectScale sc = nx::nvn::compute_target_scale(1.0f, k);
        if (c.is_wide) {
            CHECK(sc.sx > 1.0f);
            CHECK_NEAR(sc.sy, 1.0f, 1e-6);
            CHECK(sc.ax < 1.0f);
            CHECK_NEAR(sc.ay, 1.0f, 1e-6);
        } else if (c.is_tall) {
            CHECK_NEAR(sc.sx, 1.0f, 1e-6);
            CHECK(sc.sy > 1.0f);
            CHECK_NEAR(sc.ax, 1.0f, 1e-6);
            CHECK(sc.ay < 1.0f);
        } else {
            CHECK_NEAR(sc.sx, 1.0f, 1e-6);
            CHECK_NEAR(sc.sy, 1.0f, 1e-6);
            CHECK_NEAR(sc.ax, 1.0f, 1e-6);
            CHECK_NEAR(sc.ay, 1.0f, 1e-6);
        }

        // Hor+ on wide, Vert+ on tall (when vert_plus is enabled)
        const nx::nvn::PerspectiveParams p_vp = nx::nvn::compute_perspective(base_fov_y, k, true);
        const double fov_x_vp = horizontal_fov(p_vp.fov_y, p_vp.aspect);
        if (c.is_wide) {
            CHECK_NEAR(p_vp.fov_y, base_fov_y, 1e-6);
            CHECK(fov_x_vp > base_fov_x);
        } else if (c.is_tall) {
            CHECK(p_vp.fov_y > base_fov_y);
            CHECK_NEAR(fov_x_vp, base_fov_x, 1e-5);
        } else {
            CHECK_NEAR(p_vp.fov_y, base_fov_y, 1e-6);
            CHECK_NEAR(fov_x_vp, base_fov_x, 1e-5);
        }

        // When auto aspect matches the window, letterbox fills 100% of the screen
        const float ren_w = 1920.0f * sc.sx;
        const float ren_h = 1080.0f * sc.sy;
        const nx::nvn::LetterboxRect full = nx::nvn::compute_letterbox(ren_w, ren_h, float(c.w), float(c.h));
        CHECK_NEAR(full.w, 1.0f, 0.01);
        CHECK_NEAR(full.h, 1.0f, 0.01);
    }
}

void test_explicit_aspect_parsing() {
    CHECK_NEAR(nx::nvn::parse_aspect_factor("16:9", 2208, 1840), 1.0f, 1e-6);
    CHECK_NEAR(nx::nvn::parse_aspect_factor("", 2208, 1840), 1.0f, 1e-6);
    CHECK_NEAR(nx::nvn::parse_aspect_factor("21:9", 1920, 1080), 336.0f / 256.0f, 1e-5);
    CHECK_NEAR(nx::nvn::parse_aspect_factor("2560x1600", 1920, 1080), 230.0f / 256.0f, 1e-5);
    CHECK_NEAR(nx::nvn::parse_aspect_factor("window", 2092, 1080), 279.0f / 256.0f, 1e-5);
    // Clamping extremes
    CHECK_NEAR(nx::nvn::aspect_factor_from_ratio(10.0), 3.0f, 1e-6);
    CHECK_NEAR(nx::nvn::aspect_factor_from_ratio(0.2), 0.45f, 1e-2);
    CHECK_NEAR(nx::nvn::aspect_factor_from_ratio(0.0), 1.0f, 1e-6);
}

void test_safe_edge_and_ortho() {
    const float k_wide = nx::nvn::parse_aspect_factor("auto", 2092, 1080);  // 17.4:9
    const nx::nvn::SafeEdgeLayout se_wide = nx::nvn::compute_safe_edge_layout(1920.0f, 1080.0f, k_wide, false);
    CHECK_NEAR(se_wide.root_scale_x, k_wide, 1e-6);
    CHECK_NEAR(se_wide.root_scale_y, 1.0f, 1e-6);
    CHECK(se_wide.left_inset > 80.0f);
    CHECK_NEAR(se_wide.left_inset, se_wide.right_inset, 1e-5);
    CHECK_NEAR(se_wide.top_inset, 0.0f, 1e-6);

    const float k_tall = nx::nvn::parse_aspect_factor("auto", 2208, 1840);  // 6:5
    const nx::nvn::SafeEdgeLayout se_tall = nx::nvn::compute_safe_edge_layout(1920.0f, 1080.0f, k_tall, true);
    CHECK_NEAR(se_tall.root_scale_x, 1.0f, 1e-6);
    CHECK_NEAR(se_tall.root_scale_y, 1.0f / k_tall, 1e-5);
    CHECK(se_tall.top_inset > 250.0f);
    CHECK_NEAR(se_tall.top_inset, se_tall.bottom_inset, 1e-5);

    const nx::nvn::OrthoBounds base{540.0f, -540.0f, -960.0f, 960.0f};
    const nx::nvn::OrthoBounds ob_wide = nx::nvn::compute_ortho_bounds(base, k_wide);
    CHECK_NEAR(ob_wide.right - ob_wide.left, 1920.0f * k_wide, 1e-3);
    CHECK_NEAR(ob_wide.top - ob_wide.bottom, 1080.0f, 1e-3);

    const nx::nvn::OrthoBounds ob_tall = nx::nvn::compute_ortho_bounds(base, k_tall);
    CHECK_NEAR(ob_tall.right - ob_tall.left, 1920.0f, 1e-3);
    CHECK_NEAR(ob_tall.top - ob_tall.bottom, 1080.0f / k_tall, 1e-3);
}

void test_letterbox_and_pillarbox() {
    // 16:9 content on 17.4:9 folded (2092x1080) -> pillarbox
    const nx::nvn::LetterboxRect pb = nx::nvn::compute_letterbox(1920.0f, 1080.0f, 2092.0f, 1080.0f);
    CHECK_NEAR(pb.px_w, 1920, 0);
    CHECK_NEAR(pb.px_h, 1080, 0);
    CHECK_NEAR(pb.px_x, 86, 0);
    CHECK_NEAR(pb.px_y, 0, 0);

    // 16:9 content on 6:5 unfolded (2208x1840) -> letterbox
    const nx::nvn::LetterboxRect lb_fold = nx::nvn::compute_letterbox(1920.0f, 1080.0f, 2208.0f, 1840.0f);
    CHECK_NEAR(lb_fold.px_w, 2208, 0);
    CHECK_NEAR(lb_fold.px_h, 1242, 0);
    CHECK_NEAR(lb_fold.px_x, 0, 0);
    CHECK_NEAR(lb_fold.px_y, 299, 0);

    // 16:9 content on 3:2 iPad mini (2266x1488) -> letterbox
    const nx::nvn::LetterboxRect lb_ipad = nx::nvn::compute_letterbox(1920.0f, 1080.0f, 2266.0f, 1488.0f);
    CHECK_NEAR(lb_ipad.px_w, 2266, 0);
    CHECK(lb_ipad.px_h < 1488);
    CHECK(lb_ipad.px_y > 0);

    // 16:9 content on 16:10 Z13 (2560x1600) -> letterbox
    const nx::nvn::LetterboxRect lb_z13 = nx::nvn::compute_letterbox(1920.0f, 1080.0f, 2560.0f, 1600.0f);
    CHECK_NEAR(lb_z13.px_w, 2560, 0);
    CHECK_NEAR(lb_z13.px_h, 1440, 0);
    CHECK_NEAR(lb_z13.px_x, 0, 0);
    CHECK_NEAR(lb_z13.px_y, 80, 0);
}

void test_runtime_resize_transitions() {
    // Simulate fold -> unfold -> upright -> window resize
    struct Step { uint32_t w, h; };
    const Step seq[] = {
        {2092, 1080},  // folded 17.4:9
        {2208, 1840},  // unfolded 6:5
        {1840, 2208},  // upright 5:6
        {2266, 1488},  // iPad mini 3:2
        {2560, 1600},  // Z13 16:10
        {1920, 1080},  // 16:9 projector
    };
    float prev_k = 0.0f;
    for (const auto& s : seq) {
        const float k = nx::nvn::parse_aspect_factor("auto", s.w, s.h);
        CHECK(k != prev_k);
        prev_k = k;
        const nx::nvn::AspectScale sc = nx::nvn::compute_target_scale(1.0f, k);
        int32_t vx = 0, vw = 1920;
        int32_t vy = 0, vh = 1080;
        const uint32_t tw = uint32_t(std::lround(1920.0 * sc.sx));
        const uint32_t th = uint32_t(std::lround(1080.0 * sc.sy));
        nx::nvn::scale_viewport_span(vx, vw, tw, sc.sx);
        nx::nvn::scale_viewport_span(vy, vh, th, sc.sy);
        CHECK_NEAR(vx, 0, 0);
        CHECK_NEAR(vy, 0, 0);
        CHECK_NEAR(vw, int32_t(tw), 0);
        CHECK_NEAR(vh, int32_t(th), 0);
    }
}

}  // namespace

int main() {
    test_target_resolutions();
    test_explicit_aspect_parsing();
    test_safe_edge_and_ortho();
    test_letterbox_and_pillarbox();
    test_runtime_resize_transitions();
    if (g_failures) {
        std::fprintf(stderr, "%d aspect test(s) failed\n", g_failures);
        return 1;
    }
    std::printf("aspect_test: all cases passed\n");
    return 0;
}
