#pragma once

#include "mofa/pe_resources.hpp"

#include <array>
#include <cstdint>
#include <filesystem>
#include <optional>
#include <string>
#include <vector>

namespace mofa {

struct RgbaImage {
    std::uint32_t width = 0;
    std::uint32_t height = 0;
    std::vector<std::uint8_t> pixels;
};

std::optional<RgbaImage> decode_icon(const EmbeddedIcon& icon,
                                     std::string* error = nullptr);
RgbaImage fit_icon(const RgbaImage& source, std::uint32_t width,
                   std::uint32_t height,
                   std::array<std::uint8_t, 3> background = {24, 24, 28});
bool write_vita_indexed_png(const std::filesystem::path& path,
                            const RgbaImage& image,
                            std::string* error = nullptr);

} // namespace mofa

