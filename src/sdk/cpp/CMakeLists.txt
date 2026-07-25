cmake_minimum_required(VERSION 3.16)
project(mvf_sdk_cpp LANGUAGES CXX VERSION 0.1.0)

# Package version — defaults to the project() version, overridable from CI with the
# release tag: cmake -DMVF_SDK_VERSION=1.2.3
set(MVF_SDK_VERSION "${PROJECT_VERSION}" CACHE STRING "MVF C++ SDK version")

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
if(NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
  set(CMAKE_BUILD_TYPE Release CACHE STRING "" FORCE)
endif()

# nlohmann/json — single-header JSON, the SDK's only third-party dependency.
# Fetched at configure time (git). Set MVF_SDK_SYSTEM_JSON=ON to use an installed copy.
option(MVF_SDK_SYSTEM_JSON "Use a system-installed nlohmann_json instead of fetching" OFF)
if(MVF_SDK_SYSTEM_JSON)
  find_package(nlohmann_json 3.9 REQUIRED)
else()
  include(FetchContent)
  FetchContent_Declare(
    nlohmann_json
    GIT_REPOSITORY https://github.com/nlohmann/json
    GIT_TAG        v3.11.3
    GIT_SHALLOW    TRUE
  )
  FetchContent_MakeAvailable(nlohmann_json)
endif()

string(REGEX REPLACE "^([0-9]+).*" "\\1" MVF_SDK_VERSION_MAJOR "${MVF_SDK_VERSION}")

# The shared library: libmvf_sdk.so / libmvf_sdk.dylib / mvf_sdk.dll
add_library(mvf_sdk SHARED src/sdk.cpp)
add_library(mvf::sdk ALIAS mvf_sdk)

target_include_directories(mvf_sdk PUBLIC
  $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
  $<INSTALL_INTERFACE:include>)
target_link_libraries(mvf_sdk PUBLIC nlohmann_json::nlohmann_json)

set_target_properties(mvf_sdk PROPERTIES
  OUTPUT_NAME mvf_sdk
  VERSION ${MVF_SDK_VERSION}
  SOVERSION ${MVF_SDK_VERSION_MAJOR}
  # Export the mvf:: API from the shared library. On ELF/Mach-O default visibility
  # exports the symbols; WINDOWS_EXPORT_ALL_SYMBOLS does the equivalent for the DLL.
  WINDOWS_EXPORT_ALL_SYMBOLS ON)

# Example module (a thin executable that links the SDK) — also our build smoke test.
option(MVF_SDK_BUILD_EXAMPLES "Build C++ SDK example modules" ON)
if(MVF_SDK_BUILD_EXAMPLES)
  add_executable(mvf_invert_transformer examples/invert_transformer.cpp)
  target_link_libraries(mvf_invert_transformer PRIVATE mvf_sdk)
endif()

include(GNUInstallDirs)
install(TARGETS mvf_sdk
  ARCHIVE DESTINATION ${CMAKE_INSTALL_LIBDIR}
  LIBRARY DESTINATION ${CMAKE_INSTALL_LIBDIR}
  RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR})
install(DIRECTORY include/ DESTINATION ${CMAKE_INSTALL_INCLUDEDIR})
