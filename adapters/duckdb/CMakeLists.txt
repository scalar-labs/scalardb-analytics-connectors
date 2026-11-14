cmake_minimum_required(VERSION 3.24)
project(scalardb_analytics_duckdb VERSION 0.1.0 LANGUAGES CXX)

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
set(SUBSTRAIT_PROTO_DIR "$ENV{SUBSTRAIT_PROTO_DIR}" CACHE PATH
    "The proto directory of the Substrait specification the connector API pins")

# The adapter's C++ API use is tied to one DuckDB version; DuckDB's package
# reports it as "v1.5.2", which find_package cannot compare.
find_package(DuckDB REQUIRED CONFIG)
if(NOT DuckDB_VERSION STREQUAL "v1.5.2")
  message(FATAL_ERROR "the DuckDB adapter requires DuckDB v1.5.2, found ${DuckDB_VERSION}")
endif()
find_package(Protobuf CONFIG REQUIRED)

if(NOT SUBSTRAIT_PROTO_DIR)
  message(FATAL_ERROR "SUBSTRAIT_PROTO_DIR must name the Substrait proto directory")
endif()

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
# DuckDB itself is linked by the final binary: the host process provides it
# to the loadable extension, and an embedding application links it.
target_include_directories(scalardb_analytics_duckdb
  PUBLIC include "${CONNECTOR_API_INCLUDE_DIR}" ${DuckDB_INCLUDE_DIRS}
  PRIVATE src)
target_link_libraries(scalardb_analytics_duckdb PUBLIC substrait_proto)

# The extension DuckDB loads; it owns the connector API library.
add_library(scalardb_analytics_extension SHARED src/extension.cpp)
target_link_libraries(scalardb_analytics_extension PRIVATE
  scalardb_analytics_duckdb "${SCALARDB_ANALYTICS_CONNECTOR_LIBRARY}")
set_target_properties(scalardb_analytics_extension PROPERTIES
  OUTPUT_NAME scalardb_analytics
  PREFIX ""
  SUFFIX ".duckdb_extension")
if(APPLE)
  target_link_options(scalardb_analytics_extension PRIVATE -undefined dynamic_lookup)
  set(DUCKDB_PLATFORM_OS osx)
else()
  set(DUCKDB_PLATFORM_OS linux)
endif()
if(CMAKE_SYSTEM_PROCESSOR MATCHES "^(arm64|aarch64)$")
  set(DUCKDB_PLATFORM "${DUCKDB_PLATFORM_OS}_arm64")
else()
  set(DUCKDB_PLATFORM "${DUCKDB_PLATFORM_OS}_amd64")
endif()
# DuckDB loads only a file that ends with its metadata footer; CMake strings
# cannot hold the footer's NUL bytes, so a build tool writes it.
add_executable(append_metadata cmake/append_metadata.cpp)
add_custom_command(TARGET scalardb_analytics_extension POST_BUILD
  COMMAND append_metadata $<TARGET_FILE:scalardb_analytics_extension>
    ${DUCKDB_PLATFORM} ${DuckDB_VERSION} ${PROJECT_VERSION}
  VERBATIM)

find_package(Catch2 3 CONFIG REQUIRED)
enable_testing()
add_executable(scalardb_analytics_duckdb_tests
  test/arrow_schema_test.cpp
  test/attach_test.cpp
  test/plan_test.cpp
  test/postgresql_test.cpp
  test/support.cpp)
target_link_libraries(scalardb_analytics_duckdb_tests PRIVATE
  scalardb_analytics_duckdb duckdb "${SCALARDB_ANALYTICS_CONNECTOR_LIBRARY}"
  Catch2::Catch2WithMain)
# The read schema check is tested at its boundary, which no Connector can
# reach: the library builds a read's schema from the remote output itself.
target_include_directories(scalardb_analytics_duckdb_tests PRIVATE src)
add_test(NAME scalardb_analytics_duckdb_tests COMMAND scalardb_analytics_duckdb_tests)

# The loadable extension in the DuckDB CLI, end to end against PostgreSQL.
find_program(DUCKDB_CLI duckdb REQUIRED)
add_test(NAME scalardb_analytics_duckdb_e2e
  COMMAND sh "${CMAKE_CURRENT_SOURCE_DIR}/test/e2e.sh" "${DUCKDB_CLI}"
    $<TARGET_FILE:scalardb_analytics_extension>)
