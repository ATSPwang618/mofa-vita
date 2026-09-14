#pragma once

#include "mofa/game.hpp"

#include <filesystem>
#include <string>

namespace mofa {

struct BubbleSpec {
    std::string title;
    std::string title_id;
    std::string game_id;
    std::filesystem::path executable;
};

std::string bubble_title_id(const GameDescriptor& game);
bool is_vita_title_id(const std::string& title_id);
bool stage_bubble(const BubbleSpec& spec,
                  const std::filesystem::path& template_root,
                  const std::filesystem::path& staging_root,
                  std::string* error = nullptr);

#ifdef MOFA_VITA
int install_staged_bubble(const std::filesystem::path& staging_root);
#endif

} // namespace mofa
