cmake_minimum_required(VERSION 3.24)

# The adapter has no version of its own: it releases under the repository's
# one version, which the Cargo workspace holds.
execute_process(
  COMMAND cargo metadata --no-deps --format-version 1
  WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}/../.."
  OUTPUT_VARIABLE CARGO_METADATA
  COMMAND_ERROR_IS_FATAL ANY)
string(JSON CARGO_PACKAGE_COUNT LENGTH "${CARGO_METADATA}" packages)
math(EXPR CARGO_LAST_PACKAGE "${CARGO_PACKAGE_COUNT} - 1")
foreach(index RANGE ${CARGO_LAST_PACKAGE})
  string(JSON package_name GET "${CARGO_METADATA}" packages ${index} name)
  if(package_name STREQUAL "connector-api")
    string(JSON SCALARDB_ANALYTICS_VERSION GET "${CARGO_METADATA}" packages ${index} version)
  endif()
endforeach()
if(NOT SCALARDB_ANALYTICS_VERSION)
  message(FATAL_ERROR "the Cargo workspace has no connector-api package")
endif()

project(scalardb_analytics_duckdb VERSION ${SCALARDB_ANALYTICS_VERSION} LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

# The connector API library built by Cargo; its features decide which
# Connectors the tests can open.
set(SCALARDB_ANALYTICS_CONNECTOR_LIBRARY
    "${CMAKE_CURRENT_SOURCE_DIR}/../../target/debug/${CMAKE_SHARED_LIBRARY_PREFIX}connector_api${CMAKE_SHARED_LIBRARY_SUFFIX}"
    CACHE FILEPATH "The connector-api shared library that the loadable extension and the tests link")
if(NOT EXISTS "${SCALARDB_ANALYTICS_CONNECTOR_LIBRARY}")
  message(FATAL_ERROR "build the connector API library first: ${SCALARDB_ANALYTICS_CONNECTOR_LIBRARY}")
endif()
option(SCALARDB_ANALYTICS_DISTRIBUTION
  "Build only the loadable extension for the distribution package: it finds the connector API library in ../lib, carries DuckDB, protobuf and abseil inside, and takes DuckDB from its pinned release files instead of the development environment and protobuf and abseil from CMAKE_PREFIX_PATH"
  OFF)

if(SCALARDB_ANALYTICS_DISTRIBUTION)
  include(FetchContent)
  # A SOURCE_SUBDIR without a CMakeLists.txt makes FetchContent download a
  # source without building it.
  FetchContent_Declare(duckdb
    URL https://github.com/duckdb/duckdb/archive/refs/tags/v1.5.6.tar.gz
    URL_HASH SHA256=1fadcbe9e69e1470f9093b6bcde08daf477d729c449e59a807f45c346622099b
    SOURCE_SUBDIR headers-only)
  # DuckDB's static library from its release, which the extension carries: a
  # host process need not export DuckDB's C++ symbols, and DuckDB's JDBC
  # driver exports none.
  if(APPLE AND CMAKE_SYSTEM_PROCESSOR STREQUAL "arm64")
    set(DUCKDB_STATIC_ASSET osx-arm64)
    set(DUCKDB_STATIC_SHA256 e77c382fba15e3f5c0b3265d12e3a4de534699fdb91bcc9d10d4a857a58103d5)
  elseif(NOT APPLE AND CMAKE_SYSTEM_PROCESSOR STREQUAL "aarch64")
    set(DUCKDB_STATIC_ASSET linux-arm64)
    set(DUCKDB_STATIC_SHA256 56249751117a4c04289764e06eb431352ce33fb3f8e2f30401122d4d0e184139)
  elseif(NOT APPLE AND CMAKE_SYSTEM_PROCESSOR STREQUAL "x86_64")
    set(DUCKDB_STATIC_ASSET linux-amd64)
    set(DUCKDB_STATIC_SHA256 ab3d1d33951b8cb8bdcae740d8e06adf66bfb7ee880a3f7052d7a35088e16d2d)
  else()
    message(FATAL_ERROR "no DuckDB static library is pinned for ${CMAKE_SYSTEM_NAME} ${CMAKE_SYSTEM_PROCESSOR}")
  endif()
  FetchContent_Declare(duckdb_static
    URL https://github.com/duckdb/duckdb/releases/download/v1.5.6/static-libs-${DUCKDB_STATIC_ASSET}.zip
    URL_HASH SHA256=${DUCKDB_STATIC_SHA256}
    SOURCE_SUBDIR libraries-only)
  # The Substrait specification version flake.nix pins for development.
  FetchContent_Declare(substrait
    URL https://github.com/substrait-io/substrait/archive/refs/tags/v0.69.0.tar.gz
    URL_HASH SHA256=7995cd3c63cb4eed9b68f1d2bef9ab5a6688aadd1846d525be3b5518749691d1
    SOURCE_SUBDIR proto-only)
  FetchContent_MakeAvailable(duckdb duckdb_static substrait)
  set(DUCKDB_SOURCE_DIR "${duckdb_SOURCE_DIR}")
  set(DUCKDB_STATIC_DIR "${duckdb_static_SOURCE_DIR}")
  set(SUBSTRAIT_PROTO_DIR "${substrait_SOURCE_DIR}/proto")
else()
  # The same DuckDB v1.5.6 release files, unpacked by the development shell
  # (flake.nix), with the shared library and CLI the tests use beside them.
  set(DUCKDB_DIR "$ENV{DUCKDB_DIR}" CACHE PATH
      "The DuckDB v1.5.6 release files the development shell unpacks")
  if(NOT DUCKDB_DIR)
    message(FATAL_ERROR "DUCKDB_DIR must name the DuckDB release files of the development shell")
  endif()
  set(DUCKDB_SOURCE_DIR "${DUCKDB_DIR}/source")
  set(DUCKDB_STATIC_DIR "${DUCKDB_DIR}/static")
  set(SUBSTRAIT_PROTO_DIR "$ENV{SUBSTRAIT_PROTO_DIR}" CACHE PATH
      "The proto directory of the Substrait specification the connector API pins")
  if(NOT SUBSTRAIT_PROTO_DIR)
    message(FATAL_ERROR "SUBSTRAIT_PROTO_DIR must name the Substrait proto directory")
  endif()
endif()
# Static protobuf and abseil, which link into the extension: the development
# shell's, or for the distribution package the prebuilt artifact that
# packaging/package.sh puts on CMAKE_PREFIX_PATH.
find_package(Protobuf CONFIG REQUIRED)
# The adapter's C++ API use is tied to this DuckDB version.
set(DuckDB_VERSION v1.5.6)
set(DuckDB_INCLUDE_DIRS "${DUCKDB_SOURCE_DIR}/src/include")

set(CONNECTOR_API_INCLUDE_DIR
    "${CMAKE_CURRENT_SOURCE_DIR}/../../crates/connector-api/include")

# Substrait C++ types generated from the pinned specification.
file(GLOB_RECURSE SUBSTRAIT_PROTOS "${SUBSTRAIT_PROTO_DIR}/substrait/*.proto")
add_library(substrait_proto STATIC)
target_sources(substrait_proto PRIVATE ${SUBSTRAIT_PROTOS})
target_link_libraries(substrait_proto PUBLIC protobuf::libprotobuf)
set(SUBSTRAIT_GENERATED_DIR "${CMAKE_CURRENT_BINARY_DIR}/substrait")
file(MAKE_DIRECTORY "${SUBSTRAIT_GENERATED_DIR}")
protobuf_generate(
  TARGET substrait_proto
  LANGUAGE cpp
  IMPORT_DIRS "${SUBSTRAIT_PROTO_DIR}"
  PROTOC_OUT_DIR "${SUBSTRAIT_GENERATED_DIR}")
target_include_directories(substrait_proto PUBLIC "${SUBSTRAIT_GENERATED_DIR}")

# The adapter, independent of how the connector API library is obtained.
add_library(scalardb_analytics_duckdb STATIC
  src/attach.cpp
  src/catalog.cpp
  src/connector_catalog_source.cpp
  src/data_source.cpp
  src/local_plan.cpp
  src/offered_plan.cpp
  src/scan.cpp
  src/types.cpp)
# DuckDB itself is linked by the final binary: the extension carries it, and
# an embedding application links it.
target_include_directories(scalardb_analytics_duckdb
  PUBLIC include "${CONNECTOR_API_INCLUDE_DIR}" ${DuckDB_INCLUDE_DIRS}
  PRIVATE src)
target_link_libraries(scalardb_analytics_duckdb PUBLIC substrait_proto)

# The extension DuckDB loads; it owns the connector API library.
add_library(scalardb_analytics_connectors_extension SHARED src/extension.cpp)
target_link_libraries(scalardb_analytics_connectors_extension PRIVATE
  scalardb_analytics_duckdb "${SCALARDB_ANALYTICS_CONNECTOR_LIBRARY}")
set_target_properties(scalardb_analytics_connectors_extension PROPERTIES
  OUTPUT_NAME scalardb_analytics_connectors
  PREFIX ""
  SUFFIX ".duckdb_extension")
if(APPLE)
  set(DUCKDB_PLATFORM_OS osx)
  set(PACKAGE_LIBRARY_RPATH "@loader_path/../lib")
else()
  set(DUCKDB_PLATFORM_OS linux)
  set(PACKAGE_LIBRARY_RPATH "$ORIGIN/../lib")
endif()
# The extension resolves DuckDB in its own copy and exports only the entry
# point DuckDB calls, so neither its DuckDB nor its protobuf and abseil meet
# the host's. Neither the DuckDB CLI of the development shell nor DuckDB's
# JDBC driver exports DuckDB's C++ symbols. Unused code is stripped.
#
# The static library leaves DuckDB's built-in extension loader
# (ExtensionHelper::LoadAllExtensions and LoadExtension) to a separate
# library, which would pull in every built-in extension. Despite its name,
# dummy_static_extension_loader is DuckDB's own loader for this case: it
# loads no built-in extension, and DuckDB links it with libduckdb_static.a
# for its own loadable extensions (extension/extension_build_tools.cmake,
# EXTENSION_STATIC_BUILD). Its LoadAllExtensions runs only when a DuckDB
# database is opened, which the host does, and its LoadExtension makes DuckDB
# load an extension from disk instead of from the binary.
#
# The DuckDB release ships the static library without the loader, which the
# source archive holds.
find_package(Threads REQUIRED)
target_sources(scalardb_analytics_connectors_extension PRIVATE
  "${DUCKDB_SOURCE_DIR}/extension/loader/dummy_static_extension_loader.cpp")
target_link_libraries(scalardb_analytics_connectors_extension PRIVATE
  "${DUCKDB_STATIC_DIR}/libduckdb_static.a" Threads::Threads ${CMAKE_DL_LIBS})
if(APPLE)
  target_link_options(scalardb_analytics_connectors_extension PRIVATE
    "LINKER:-dead_strip"
    "LINKER:-exported_symbol,_scalardb_analytics_connectors_duckdb_cpp_init")
else()
  # GNU ld links a shared library with undefined symbols, which then fail
  # only when DuckDB loads it; --no-undefined fails the link instead, as
  # Apple's linker does by default.
  target_link_options(scalardb_analytics_connectors_extension PRIVATE
    "LINKER:--gc-sections"
    "LINKER:--no-undefined"
    "LINKER:--version-script=${CMAKE_CURRENT_SOURCE_DIR}/cmake/extension.map")
endif()
if(SCALARDB_ANALYTICS_DISTRIBUTION)
  # The package's lib directory, and not the directory it was built in.
  set_target_properties(scalardb_analytics_connectors_extension PROPERTIES
    BUILD_WITH_INSTALL_RPATH ON
    INSTALL_RPATH "${PACKAGE_LIBRARY_RPATH}")
endif()
if(CMAKE_SYSTEM_PROCESSOR MATCHES "^(arm64|aarch64)$")
  set(DUCKDB_PLATFORM "${DUCKDB_PLATFORM_OS}_arm64")
else()
  set(DUCKDB_PLATFORM "${DUCKDB_PLATFORM_OS}_amd64")
endif()
# DuckDB loads only a file that ends with its metadata footer; CMake strings
# cannot hold the footer's NUL bytes, so a build tool writes it.
add_executable(append_metadata cmake/append_metadata.cpp)
add_custom_command(TARGET scalardb_analytics_connectors_extension POST_BUILD
  COMMAND append_metadata $<TARGET_FILE:scalardb_analytics_connectors_extension>
    ${DUCKDB_PLATFORM} ${DuckDB_VERSION} ${PROJECT_VERSION}
  VERBATIM)

if(SCALARDB_ANALYTICS_DISTRIBUTION)
  # The package is tested by unpacking it (packaging/test/check-package.sh).
  return()
endif()

find_package(Catch2 3 CONFIG REQUIRED)
enable_testing()
# The tests embed DuckDB through its shared library from the release.
add_library(duckdb SHARED IMPORTED)
set_target_properties(duckdb PROPERTIES
  IMPORTED_LOCATION "${DUCKDB_DIR}/lib/libduckdb${CMAKE_SHARED_LIBRARY_SUFFIX}")
add_executable(scalardb_analytics_duckdb_tests
  test/arrow_schema_test.cpp
  test/attach_test.cpp
  test/extension_test.cpp
  test/plan_test.cpp
  test/postgresql_test.cpp
  test/support.cpp)
target_link_libraries(scalardb_analytics_duckdb_tests PRIVATE
  scalardb_analytics_duckdb duckdb "${SCALARDB_ANALYTICS_CONNECTOR_LIBRARY}"
  Catch2::Catch2WithMain)
# The read schema check is tested at its boundary, which no Connector can
# reach: the library builds a read's schema from the remote output itself.
target_include_directories(scalardb_analytics_duckdb_tests PRIVATE src)
# The loadable extension is also loaded into the test process, which
# initialized the library before, as an application that owns it does.
target_compile_definitions(scalardb_analytics_duckdb_tests PRIVATE
  SCALARDB_ANALYTICS_EXTENSION_FILE="$<TARGET_FILE:scalardb_analytics_connectors_extension>")
add_dependencies(scalardb_analytics_duckdb_tests scalardb_analytics_connectors_extension)
add_test(NAME scalardb_analytics_duckdb_tests COMMAND scalardb_analytics_duckdb_tests)

# The loadable extension in the DuckDB CLI, end to end against PostgreSQL.
find_program(DUCKDB_CLI duckdb REQUIRED)
add_test(NAME scalardb_analytics_duckdb_e2e
  COMMAND sh "${CMAKE_CURRENT_SOURCE_DIR}/test/e2e.sh" "${DUCKDB_CLI}"
    $<TARGET_FILE:scalardb_analytics_connectors_extension>)
