#include "bc7enc.h"
#include <algorithm>

extern "C" void knife_bc7_init() { bc7enc_compress_block_init(); }

// The Rust caller validates the RGBA and output lengths and initializes the shared read-only tables once.
extern "C" void knife_bc7_encode(const uint8_t *rgba, uint32_t width, uint32_t height, uint8_t *output) {
    bc7enc_compress_block_params params;
    bc7enc_compress_block_params_init(&params);
    params.m_max_partitions = 16;
    for (uint32_t y = 0; y < height; y += 4) {
        for (uint32_t x = 0; x < width; x += 4) {
            color_rgba block[16];
            for (uint32_t by = 0; by < 4; ++by) {
                for (uint32_t bx = 0; bx < 4; ++bx) {
                    const size_t offset = (size_t(std::min(y + by, height - 1)) * width + std::min(x + bx, width - 1)) * 4;
                    memcpy(block[by * 4 + bx].m_c, rgba + offset, 4);
                }
            }
            bc7enc_compress_block(output, block, &params);
            output += 16;
        }
    }
}
