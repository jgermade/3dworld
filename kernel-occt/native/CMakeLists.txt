cmake_minimum_required(VERSION 3.15)
project(w3d_occt LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

if(EMSCRIPTEN)
    message(STATUS "Building w3d_occt for WebAssembly via Emscripten")
    add_compile_options(-O3 -fPIC)
    add_link_options(-O3 -s WASM=1 -s ALLOW_MEMORY_GROWTH=1)
endif()

if(NOT OpenCASCADE_FOUND)
    find_package(OpenCASCADE REQUIRED)
endif()

add_library(w3d_occt STATIC w3d_occt.cpp)

target_include_directories(w3d_occt PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}
    ${OpenCASCADE_INCLUDE_DIRS}
)

if(OpenCASCADE_LIBRARIES)
    target_link_libraries(w3d_occt PRIVATE ${OpenCASCADE_LIBRARIES})
endif()
