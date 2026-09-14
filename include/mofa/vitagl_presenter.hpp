#pragma once

#include <cstdint>

#include "mofa/frame_damage.hpp"

// Presentation boundary used by the wholesale Kirikiri engine. The stable
// default path uploads Yuri's completed software framebuffer to VitaGL. The
// direct texture entry point is reserved for the disabled experimental OpenGL
// compositor. SDL is not involved.
bool mofa_vitagl_initialize();
bool mofa_vitagl_resize(int width, int height);
bool mofa_vitagl_present(const void* pixels, int pitch, int width, int height);
bool mofa_vitagl_present_damage(
    const void* pixels, int pitch, int width, int height,
    const mofa::FrameDamageRegion& damage);
// Redraws the last successfully uploaded software surface, for example after
// cursor movement, without copying the 1280x960 Kirikiri framebuffer again.
bool mofa_vitagl_redraw();
#ifdef MOFA_YURI_OPENGL_COMPOSITOR
bool mofa_vitagl_present_texture(unsigned int texture, int width, int height,
                                     int internal_width, int internal_height,
                                     float scale_width, float scale_height);
#endif
void mofa_vitagl_set_cursor(int x, int y, bool visible);
std::uint32_t mofa_vitagl_presented_frames();
std::uint32_t mofa_vitagl_contentful_frames();
std::uint32_t mofa_vitagl_uploaded_frames();
std::uint32_t mofa_vitagl_full_uploads();
std::uint32_t mofa_vitagl_partial_uploads();
std::uint64_t mofa_vitagl_uploaded_pixels();
