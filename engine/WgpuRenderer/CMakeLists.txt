# WgpuRenderer - modern Rust/wgpu graphics backend
#
# Two halves:
#   rust/        the `wgpu_renderer` crate, built as a cdylib via corrosion
#   *.cpp        the C++ Engine backend that drives the crate over the C ABI
#                (include/wgpu_renderer.hpp)

cmake_minimum_required(VERSION 3.25)

if(NOT DEFINED Rust_CARGO_TARGET)
    if(WIN32)
        if(CMAKE_CXX_COMPILER_FRONTEND_VARIANT STREQUAL "GNU")
            set(Rust_CARGO_TARGET "x86_64-pc-windows-gnu")
        else()
            set(Rust_CARGO_TARGET "x86_64-pc-windows-msvc")
        endif()
    else()
        set(Rust_CARGO_TARGET "x86_64-unknown-linux-gnu")
    endif()
endif()

# FetchContent is corrosion's recommended integration approach
include(FetchContent)
FetchContent_Declare(
    Corrosion
    GIT_REPOSITORY https://github.com/corrosion-rs/corrosion.git
    GIT_TAG v0.6.1
)
FetchContent_MakeAvailable(Corrosion)

# Profile names mirror rust/Cargo.toml (which mirrors Trident).
if(CMAKE_BUILD_TYPE STREQUAL "Debug")
    set(_wgr_cargo_profile dbg)
elseif(CMAKE_BUILD_TYPE STREQUAL "RelWithDebInfo")
    set(_wgr_cargo_profile rwdi)
else()
    set(_wgr_cargo_profile rel)
endif()

corrosion_import_crate(
    MANIFEST_PATH ${CMAKE_CURRENT_SOURCE_DIR}/rust/Cargo.toml
    CRATE_TYPES cdylib
    PROFILE ${_wgr_cargo_profile}
)

add_library(WgpuRenderer STATIC
    EngineWgpu.cpp
    GraphicsBackendWgpu.cpp
    TerrainWgpu.cpp
    WaterWgpu.cpp
    TextureWgpu.cpp
    TextureBankWgpu.cpp
    CdlodDriver.hpp
    EngineWgpu.hpp
    TerrainWgpu.hpp
    WaterWgpu.hpp
    TextureWgpu.hpp
    TextureBankWgpu.hpp
)

target_include_directories(WgpuRenderer PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}/include
)

target_link_libraries(WgpuRenderer PUBLIC Poseidon PRIVATE wgpu_renderer)

set_target_properties(WgpuRenderer PROPERTIES
    CXX_STANDARD 20
    CXX_STANDARD_REQUIRED ON
    OUTPUT_NAME "WgpuRenderer"
)

function(wgpu_renderer_stage_dll APP_TARGET)
    set(_stamp ${CMAKE_CURRENT_BINARY_DIR}/${APP_TARGET}_stage_wgpu.stamp)
    # The cdylib's PDB (dbg/rwdi only — absent in rel) sits next to the .dll byproduct.
    # Deploy it alongside so sampling profilers (Superluminal) resolve Rust symbols.
    set(_wgpu_pdb $<TARGET_FILE_DIR:wgpu_renderer-shared>/wgpu_renderer.pdb)
    set(_copy_if_exists ${CMAKE_SOURCE_DIR}/cmake/CopyIfExists.cmake)
    add_custom_command(
        OUTPUT ${_stamp}
        COMMAND ${CMAKE_COMMAND} -E copy_if_different
            $<TARGET_FILE:wgpu_renderer-shared> $<TARGET_FILE_DIR:${APP_TARGET}>
        COMMAND ${CMAKE_COMMAND} -E make_directory ${DIST_DIR}
        COMMAND ${CMAKE_COMMAND} -E copy_if_different
            $<TARGET_FILE:wgpu_renderer-shared> ${DIST_DIR}
        COMMAND ${CMAKE_COMMAND} -DSRC=${_wgpu_pdb} -DDST=$<TARGET_FILE_DIR:${APP_TARGET}> -P ${_copy_if_exists}
        COMMAND ${CMAKE_COMMAND} -DSRC=${_wgpu_pdb} -DDST=${DIST_DIR} -P ${_copy_if_exists}
        COMMAND ${CMAKE_COMMAND} -E touch ${_stamp}
        DEPENDS $<TARGET_FILE:wgpu_renderer-shared> wgpu_renderer-shared ${APP_TARGET}
        COMMENT "Staging wgpu_renderer cdylib for ${APP_TARGET}"
        VERBATIM
    )
    add_custom_target(${APP_TARGET}_stage_wgpu ALL DEPENDS ${_stamp})

    # The Rust cdylib ships beside each game executable.  Linux's dynamic loader
    # does not search the executable directory by default, so make that packaged
    # layout an explicit runtime contract instead of relying on a build-tree path.
    if(UNIX AND NOT APPLE)
        set_property(TARGET ${APP_TARGET} APPEND PROPERTY BUILD_RPATH "$ORIGIN")
        set_property(TARGET ${APP_TARGET} APPEND PROPERTY INSTALL_RPATH "$ORIGIN")
    endif()
endfunction()
