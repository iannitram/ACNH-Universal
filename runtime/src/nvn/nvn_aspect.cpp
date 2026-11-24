#include "nvn_aspect.h"

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <string_view>

namespace nx::nvn {

float aspect_factor_from_ratio(double aspect) {
    if (!(aspect > 0.0) || !std::isfinite(aspect)) return 1.0f;
    double k = std::clamp(aspect / kBaseAspect169, kMinAspectFactor, kMaxAspectFactor);
    if (std::fabs(k - 1.0) < 0.02) k = 1.0;
    k = std::round(k * 256.0) / 256.0;
    return float(k);
}

float parse_aspect_factor(const char* setting, uint32_t win_w, uint32_t win_h) {
    double aspect = kBaseAspect169;
    if (setting && setting[0]) {
        const std::string_view sv(setting);
        double n = 0.0, d = 0.0;
        if (sv == "auto" || sv == "window" || sv == "1") {
            if (win_w > 0 && win_h > 0) aspect = double(win_w) / double(win_h);
        } else if ((std::sscanf(setting, "%lf:%lf", &n, &d) == 2 || std::sscanf(setting, "%lfx%lf", &n, &d) == 2) &&
                   n > 0.0 && d > 0.0) {
            aspect = n / d;
        }
    }
    return aspect_factor_from_ratio(aspect);
}

AspectScale compute_target_scale(float s, float k) {
    const float safe_s = (s > 0.0f && std::isfinite(s)) ? s : 1.0f;
    const float safe_k = (k > 0.0f && std::isfinite(k)) ? k : 1.0f;
    AspectScale out;
    out.sx = safe_s * std::max(1.0f, safe_k);
    out.sy = safe_s * std::max(1.0f, 1.0f / safe_k);
    out.ax = out.sy < out.sx ? out.sy / out.sx : 1.0f;
    out.ay = out.sx < out.sy ? out.sx / out.sy : 1.0f;
    return out;
}

void scale_viewport_span(int32_t& x, int32_t& w, uint32_t extent, float f) {
    const int64_t x0 = std::llround(double(x) * double(f));
    int64_t ww = std::llround(double(std::max(0, w)) * double(f));
    if (std::llabs(int64_t(extent) - ww) <= 2 && x == 0) ww = extent;
    if (x0 + ww > int64_t(extent)) ww = std::max<int64_t>(0, int64_t(extent) - x0);
    x = int32_t(x0);
    w = int32_t(ww);
}

PerspectiveParams compute_perspective(float base_fov_y, float k, bool vert_plus) {
    const float safe_k = (k > 0.0f && std::isfinite(k)) ? k : 1.0f;
    PerspectiveParams out;
    out.aspect = float(kBaseAspect169 * double(safe_k));
    out.fov_y = (safe_k < 1.0f && vert_plus) ? 2.0f * std::atan(std::tan(base_fov_y * 0.5f) / safe_k) : base_fov_y;
    return out;
}

OrthoBounds compute_ortho_bounds(const OrthoBounds& base, float k) {
    const float safe_k = (k > 0.0f && std::isfinite(k)) ? k : 1.0f;
    const float kx = std::max(1.0f, safe_k);
    const float ky = std::max(1.0f, 1.0f / safe_k);
    const float cy = (base.top + base.bottom) * 0.5f;
    const float cx = (base.left + base.right) * 0.5f;
    return {
        cy + (base.top - cy) * ky,
        cy + (base.bottom - cy) * ky,
        cx + (base.left - cx) * kx,
        cx + (base.right - cx) * kx,
    };
}

SafeEdgeLayout compute_safe_edge_layout(float base_w, float base_h, float k, bool tall_edges) {
    const float safe_k = (k > 0.0f && std::isfinite(k)) ? k : 1.0f;
    SafeEdgeLayout out;
    out.root_scale_x = std::max(1.0f, safe_k);
    out.root_scale_y = tall_edges ? std::max(1.0f, 1.0f / safe_k) : 1.0f;
    out.left_inset = out.right_inset = std::max(0.0f, base_w * (out.root_scale_x - 1.0f) * 0.5f);
    out.top_inset = out.bottom_inset = std::max(0.0f, base_h * (out.root_scale_y - 1.0f) * 0.5f);
    return out;
}

LetterboxRect compute_letterbox(float src_w, float src_h, float dst_w, float dst_h) {
    LetterboxRect out;
    if (!(src_w > 0.0f && src_h > 0.0f && dst_w > 0.0f && dst_h > 0.0f)) {
        out.px_w = std::max(0, int32_t(dst_w));
        out.px_h = std::max(0, int32_t(dst_h));
        return out;
    }
    const float fit = std::min(dst_w / src_w, dst_h / src_h);
    const float bw = src_w * fit / dst_w;
    const float bh = src_h * fit / dst_h;
    out.x = (1.0f - bw) * 0.5f;
    out.y = (1.0f - bh) * 0.5f;
    out.w = bw;
    out.h = bh;
    out.px_w = std::min(int32_t(dst_w), int32_t(src_w * fit + 0.5f));
    out.px_h = std::min(int32_t(dst_h), int32_t(src_h * fit + 0.5f));
    out.px_x = (int32_t(dst_w) - out.px_w) / 2;
    out.px_y = (int32_t(dst_h) - out.px_h) / 2;
    return out;
}

}  // namespace nx::nvn
