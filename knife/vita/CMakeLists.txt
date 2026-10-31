cmake_minimum_required(VERSION 3.20)
project(knife_vita_native C)
if(NOT VITA)
    message(FATAL_ERROR "Use the VitaSDK toolchain for this native library")
endif()
# Authored Cg, embedded startup compiler input, and cache identity must agree.
find_program(KNIFE_PYTHON python3 REQUIRED)
execute_process(COMMAND "${KNIFE_PYTHON}" "${CMAKE_CURRENT_SOURCE_DIR}/../../tools/generate-runtime-shaders.py" --check
    RESULT_VARIABLE KNIFE_RUNTIME_SOURCE_STATUS)
if(NOT KNIFE_RUNTIME_SOURCE_STATUS EQUAL 0)
    message(FATAL_ERROR "Runtime shader source is stale; regenerate it before building")
endif()
# Cargo owns the only entry point and final ELF/SELF link.
add_library(knife_vita_platform STATIC platform.c gxm_backend.c image_decode.c jpeg_hw.c audio_decode.c audio_output.c movie_player.c perf_audit.c runtime_shaders.c shader_extensions.c)
# Pin the compiler extension and target libraries independently of the host SDK.
set(KNIFE_SHACCCG_EXT "${CMAKE_CURRENT_SOURCE_DIR}/vendor/shacccg-ext/libSceShaccCgExt.a")
file(SHA256 "${KNIFE_SHACCCG_EXT}" KNIFE_SHACCCG_EXT_SHA256)
if(NOT KNIFE_SHACCCG_EXT_SHA256 STREQUAL "2707ddf51699b95b9b8b45ac806b1521318ddaf9500aaf07c3db6d1edb710cdf")
    message(FATAL_ERROR "Bundled SceShaccCgExt archive is missing or modified")
endif()
add_library(knife_shacccg_ext STATIC IMPORTED)
set_target_properties(knife_shacccg_ext PROPERTIES IMPORTED_LOCATION "${KNIFE_SHACCCG_EXT}")
option(KNIFE_VITA_DEV_DIAGNOSTICS "Record native GXM lifecycle stages in the isolated DEV directory" OFF)
option(KNIFE_VITA_QUIET "Suppress engine console diagnostics" OFF)
if(KNIFE_VITA_QUIET)
    target_compile_definitions(knife_vita_platform PRIVATE KV_VITA_QUIET=1)
endif()
if(KNIFE_VITA_DEV_DIAGNOSTICS)
    target_compile_definitions(knife_vita_platform PRIVATE KV_GXM_DEV_DIAGNOSTICS=1)
endif()
include("${CMAKE_CURRENT_SOURCE_DIR}/vendor/dependencies/libraries.cmake")
target_link_libraries(knife_vita_platform PUBLIC ${KNIFE_VITA_BUNDLED_LIBRARIES} m knife_shacccg_ext)
target_include_directories(knife_vita_platform BEFORE PRIVATE
    "${CMAKE_CURRENT_SOURCE_DIR}/vendor/dependencies/include"
    "${CMAKE_CURRENT_SOURCE_DIR}/vendor/dependencies/include/opus")
target_compile_features(knife_vita_platform PRIVATE c_std_11)
target_compile_options(knife_vita_platform PRIVATE -Wall -Wextra -Werror -fno-common -ffunction-sections -fdata-sections)
# Use virtual paths in __FILE__, assertions and debug data on every host.
get_filename_component(KNIFE_SOURCE_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../.." REALPATH)
foreach(KNIFE_PATH_MAP IN ITEMS
    "$ENV{HOME}=/home/builder"
    "${KNIFE_SOURCE_ROOT}=/source"
    "$ENV{VITASDK}=/vitasdk"
    "${CMAKE_BINARY_DIR}=/build/native")
    if(NOT KNIFE_PATH_MAP MATCHES "^=")
        target_compile_options(knife_vita_platform PRIVATE
            "-ffile-prefix-map=${KNIFE_PATH_MAP}" "-fmacro-prefix-map=${KNIFE_PATH_MAP}")
    endif()
endforeach()
