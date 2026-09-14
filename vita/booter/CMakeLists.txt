cmake_minimum_required(VERSION 3.20)
project(mofa-booter LANGUAGES C CXX)

if(NOT VITA)
    message(FATAL_ERROR "Configure with the VitaSDK toolchain")
endif()
if(NOT DEFINED MOFA_ROOT)
    get_filename_component(MOFA_ROOT "${CMAKE_CURRENT_LIST_DIR}/../.." ABSOLUTE)
endif()

include("$ENV{VITASDK}/share/vita.cmake" REQUIRED)

add_executable(mofa-booter
    "${MOFA_ROOT}/src/vita/booter.cpp"
)
target_compile_features(mofa-booter PRIVATE cxx_std_14)
target_compile_options(mofa-booter PRIVATE -Wall -Wextra -Wpedantic)
target_link_libraries(mofa-booter
    SceAppMgr_stub
    SceProcessmgr_stub
)

vita_create_self(mofa-booter.self mofa-booter UNSAFE)

set(MOFA_TEMPLATE "${CMAKE_CURRENT_BINARY_DIR}/template")
add_custom_command(
    OUTPUT "${MOFA_TEMPLATE}/eboot.bin"
    COMMAND "${CMAKE_COMMAND}" -E make_directory
        "${MOFA_TEMPLATE}/sce_sys/livearea/contents"
    COMMAND "${CMAKE_COMMAND}" -E copy
        "${CMAKE_CURRENT_BINARY_DIR}/mofa-booter.self"
        "${MOFA_TEMPLATE}/eboot.bin"
    COMMAND "${CMAKE_COMMAND}" -E copy_directory
        "${MOFA_ROOT}/resources/vita/sce_sys/livearea/contents"
        "${MOFA_TEMPLATE}/sce_sys/livearea/contents"
    DEPENDS mofa-booter.self-self
            "${MOFA_ROOT}/resources/vita/sce_sys/livearea/contents/template.xml"
            "${MOFA_ROOT}/resources/vita/sce_sys/livearea/contents/background.png"
            "${MOFA_ROOT}/resources/vita/sce_sys/livearea/contents/startup.png"
    VERBATIM
)
add_custom_target(mofa-booter-template ALL
    DEPENDS "${MOFA_TEMPLATE}/eboot.bin")
