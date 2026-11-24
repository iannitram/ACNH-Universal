// Pure aspect, projection, safe-edge, and letterbox math shared between the NVN runtime and unit tests.
// Zero dependencies on Vulkan, SDL, Android, or guest memory.
#pragma once

#include <cstdint>

namespace nx::nvn {

inline constexpr double kBaseAspect169 = 16.0 / 9.0;
inline constexpr double kMinAspectFactor = 0.45;  // allows Pixel Fold upright inner screen (5:6 -> k ~ 0.469)
inline constexpr double kMaxAspectFactor = 3.0;

struct AspectScale {
    float sx = 1.0f;  // horizontal render-target scale: s * max(1, k)
    float sy = 1.0f;  // vertical render-target scale:   s * max(1, 1/k)
    float ax = 1.0f;  // horizontal UI squeeze <= 1 (1/k when wide, 1 otherwise)
    float ay = 1.0f;  // vertical UI squeeze <= 1 (k when tall, 1 otherwise)
};

struct PerspectiveParams {
    float aspect = float(kBaseAspect169);
    float fov_y = 0.0f;
};

struct OrthoBounds {
    float top = 540.0f;
    float bottom = -540.0f;
    float left = -960.0f;
    float right = 960.0f;
};

struct SafeEdgeLayout {
    float root_scale_x = 1.0f;
    float root_scale_y = 1.0f;
    float left_inset = 0.0f;    // horizontal expansion on each side in layout units
    float right_inset = 0.0f;
    float top_inset = 0.0f;     // vertical expansion on each edge in layout units
    float bottom_inset = 0.0f;
};

struct LetterboxRect {
    float x = 0.0f;  // normalized 0..1 box in destination target
    float y = 0.0f;
    float w = 1.0f;
    float h = 1.0f;
    int32_t px_x = 0;  // integer pixel rect in destination target
    int32_t px_y = 0;
    int32_t px_w = 0;
    int32_t px_h = 0;
};

// Aspect factor k = clamp(aspect / (16/9), 0.45, 3.0), snapped to 1.0 within 2%, quantized to 1/256.
float aspect_factor_from_ratio(double aspect);

// Parse video.aspect ("auto", "window", "1", "W:H", "WxH", or fallback 16:9) against window dimensions.
float parse_aspect_factor(const char* setting, uint32_t win_w, uint32_t win_h);

// Target scale factors and UI squeeze for resolution scale `s` and aspect factor `k`.
AspectScale compute_target_scale(float s, float k);

// Scale a guest viewport/scissor span `(x, w)` by `f`, clamped inside `extent`.
void scale_viewport_span(int32_t& x, int32_t& w, uint32_t extent, float f);

// Perspective projection: Hor+ on wide (k >= 1), Vert+ on tall (k < 1) when `vert_plus` is true.
PerspectiveParams compute_perspective(float base_fov_y, float k, bool vert_plus);

// Orthographic 2D projection widened/heightened about its center.
OrthoBounds compute_ortho_bounds(const OrthoBounds& base, float k);

// Safe-edge expansion for a 16:9 UI root pane (1920x1080 or 1280x720) at aspect factor `k`.
SafeEdgeLayout compute_safe_edge_layout(float base_w, float base_h, float k, bool tall_edges = false);

// Fit a source picture `(src_w, src_h)` into `(dst_w, dst_h)` keeping aspect ratio (letterbox or pillarbox).
LetterboxRect compute_letterbox(float src_w, float src_h, float dst_w, float dst_h);

}  // namespace nx::nvn
