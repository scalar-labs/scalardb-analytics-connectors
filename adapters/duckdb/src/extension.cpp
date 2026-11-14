// The loadable extension: DuckDB loads it, and it owns the connector API
// library, which it initializes on load and never shuts down because DuckDB
// does not unload extensions.
//
// The ADBC driver directory comes from the environment variable
// SCALARDB_ANALYTICS_ADBC_DRIVER_DIRECTORY, because DuckDB applies an
// extension's own settings only after loading it.
#include <cstdlib>
#include <mutex>

#include "connector_api.hpp"
#include "duckdb/main/extension/extension_loader.hpp"
#include "scalardb_analytics_duckdb.hpp"

#include "register.hpp"

extern "C" {

DUCKDB_CPP_EXTENSION_ENTRY(scalardb_analytics, loader) {
	auto &api = *scalardb_analytics_connector_api();
	// Every database of the process loads the extension; the library is
	// initialized once for the process.
	static std::once_flag initialized;
	std::call_once(initialized, [&api]() {
		ScalardbAnalyticsConnectorLibrarySettings settings {std::getenv("SCALARDB_ANALYTICS_ADBC_DRIVER_DIRECTORY")};
		scalardb_analytics_duckdb::Check(api, api.library_initialize(&settings), "initialize the connector library");
	});
	scalardb_analytics_duckdb::RegisterWith(loader, api);
}
}
