// The loadable extension: DuckDB loads it, and it initializes the connector
// API library on load unless the library is already initialized in the
// process. It never shuts the library down, because DuckDB does not unload
// extensions.
//
// The ADBC driver directory comes from the environment variable
// SCALARDB_ANALYTICS_ADBC_DRIVER_DIRECTORY, because DuckDB applies an
// extension's own settings only after loading it. Without it, the directory is
// the distribution package's `adbc-drivers`, beside the extension's own
// directory.
#include <dlfcn.h>

#include <cstdlib>
#include <filesystem>
#include <functional>
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

// Initializes the library for the process, unless another caller in the
// process already owns it.
//
// The second case is an application that owns the library and runs DuckDB
// through a binding that can only load extensions, such as a JDBC or Python
// driver, and so cannot use the embedding entry (`Register`). The application
// initializes the library with its own settings before loading the extension,
// shares it with its other callers, and shuts it down after closing DuckDB; the
// extension then only uses it. Its own driver directory is not used, and it
// does not shut the library down in either case, so the owner keeps both the
// settings and the shutdown.
void InitializeLibrary(const ScalardbAnalyticsConnectorApi &api) {
	if (api.library_is_initialized()) {
		return;
	}
	const char *configured = std::getenv("SCALARDB_ANALYTICS_ADBC_DRIVER_DIRECTORY");
	std::string directory = configured ? configured : PackagedDriverDirectory();
	ScalardbAnalyticsConnectorLibrarySettings settings {directory.empty() ? nullptr : directory.c_str()};
	scalardb_analytics_duckdb::Check(api, api.library_initialize(&settings), "initialize the connector library");
}

} // namespace

extern "C" {

DUCKDB_CPP_EXTENSION_ENTRY(scalardb_analytics_connectors, loader) {
	auto &api = *scalardb_analytics_connector_api();
	// Every database of the process loads the extension; the library is
	// initialized at most once for the process.
	static std::once_flag initialized;
	std::call_once(initialized, InitializeLibrary, std::cref(api));
	scalardb_analytics_duckdb::RegisterWith(loader, api);
}
}
