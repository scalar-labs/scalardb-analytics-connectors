// Registration shared by the loadable extension and embedding applications.
#pragma once

#include "duckdb/main/extension/extension_loader.hpp"
#include "scalardb_analytics_connector.h"

namespace scalardb_analytics_duckdb {

// Registers the attach type and the bulk-attach pragma through `loader`.
void RegisterWith(duckdb::ExtensionLoader &loader, const ScalardbAnalyticsConnectorApi &api);

} // namespace scalardb_analytics_duckdb
