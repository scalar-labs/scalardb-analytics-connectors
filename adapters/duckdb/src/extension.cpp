// The loadable extension: DuckDB loads it, and it owns the connector API
// library, which it initializes on load and never shuts down because DuckDB
// does not unload extensions.
//
// The ADBC driver directory comes from the environment variable
// SCALARDB_ANALYTICS_ADBC_DRIVER_DIRECTORY, because DuckDB applies an
// extension's own settings only after loading it. Without it, the directory is
// the distribution package's `adbc-drivers`, beside the extension's own
// directory.
#include <dlfcn.h>

#include <cstdlib>
#include <filesystem>
#include <mutex>
#include <string>

#include "connector_api.hpp"
#include "duckdb/main/extension/extension_loader.hpp"
#include "scalardb_analytics_duckdb.hpp"

#include "register.hpp"

namespace {

// The `adbc-drivers` directory of the package this extension file is in, or
// an empty string when the file cannot be located.
std::string PackagedDriverDirectory() {
	Dl_info info;
	if (dladdr(reinterpret_cast<const void *>(&PackagedDriverDirectory), &info) == 0 || !info.dli_fname) {
		return {};
	}
	auto extension = std::filesystem::absolute(info.dli_fname);
	return (extension.parent_path().parent_path() / "adbc-drivers").string();
}

} // namespace

extern "C" {

DUCKDB_CPP_EXTENSION_ENTRY(scalardb_analytics_connectors, loader) {
	auto &api = *scalardb_analytics_connector_api();
	// Every database of the process loads the extension; the library is
	// initialized once for the process.
	static std::once_flag initialized;
	std::call_once(initialized, [&api]() {
		const char *configured = std::getenv("SCALARDB_ANALYTICS_ADBC_DRIVER_DIRECTORY");
		std::string directory = configured ? configured : PackagedDriverDirectory();
		ScalardbAnalyticsConnectorLibrarySettings settings {directory.empty() ? nullptr : directory.c_str()};
		scalardb_analytics_duckdb::Check(api, api.library_initialize(&settings), "initialize the connector library");
	});
	scalardb_analytics_duckdb::RegisterWith(loader, api);
}
}
