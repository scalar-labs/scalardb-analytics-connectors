// The adapter's DuckDB settings: how long connector API requests may take.
//
// Each is a number of milliseconds, and 0, the default, sets no limit. They
// are DuckDB settings rather than ATTACH options, because ATTACH options are
// the Connector's own, typed by its declarations.
#pragma once

#include <cstdint>

#include "duckdb/main/client_context.hpp"
#include "duckdb/main/config.hpp"

namespace scalardb_analytics_duckdb {

// Limits connecting while a Data Source and its sessions open; read when the
// Data Source is attached, since opening cannot be cancelled.
constexpr const char *CONNECTION_TIMEOUT = "scalardb_analytics_connection_timeout";
// Limits each catalog request (namespace and table lists and descriptions);
// read when the Data Source is attached.
constexpr const char *METADATA_TIMEOUT = "scalardb_analytics_metadata_timeout";
// Limits each read from its first pull to its end; read when the scan starts.
constexpr const char *READ_TIMEOUT = "scalardb_analytics_read_timeout";

// Declares the settings on a database.
inline void AddSettings(duckdb::DBConfig &config) {
	config.AddExtensionOption(CONNECTION_TIMEOUT,
	                          "Milliseconds a Data Source may take to connect when it opens; 0 sets no limit",
	                          duckdb::LogicalType::UBIGINT, duckdb::Value::UBIGINT(0));
	config.AddExtensionOption(METADATA_TIMEOUT,
	                          "Milliseconds each catalog request to a Data Source may take; 0 sets no limit",
	                          duckdb::LogicalType::UBIGINT, duckdb::Value::UBIGINT(0));
	config.AddExtensionOption(READ_TIMEOUT,
	                          "Milliseconds each read from a Data Source may take from its first row; 0 sets no limit",
	                          duckdb::LogicalType::UBIGINT, duckdb::Value::UBIGINT(0));
}

// The current value of one of the settings.
inline uint64_t Milliseconds(duckdb::ClientContext &context, const char *setting) {
	duckdb::Value value;
	if (!context.TryGetCurrentSetting(setting, value) || value.IsNull()) {
		return 0;
	}
	return value.GetValue<uint64_t>();
}

} // namespace scalardb_analytics_duckdb
