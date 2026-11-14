// Helpers for calling the connector API.
#pragma once

#include <string>
#include <vector>

#include "duckdb/common/exception.hpp"
#include "scalardb_analytics_connector.h"

namespace scalardb_analytics_duckdb {

// Copies text the library returned; absent text becomes empty.
inline std::string Text(ScalardbAnalyticsConnectorString value) {
	return value.data ? std::string(value.data, value.length) : std::string();
}

// Copies a name sequence the library returned.
inline std::vector<std::string> NamePath(const ScalardbAnalyticsConnectorNames &names) {
	std::vector<std::string> result;
	for (size_t i = 0; i < names.name_count; i++) {
		result.push_back(Text(names.names[i]));
	}
	return result;
}

// A failed connector API call as a DuckDB exception that keeps the error's
// kind, so that the adapter can act on it.
class ConnectorException : public duckdb::IOException {
public:
	ConnectorException(ScalardbAnalyticsConnectorErrorKind kind_p, const std::string &message)
	    : duckdb::IOException(message), kind(kind_p) {
	}

	ScalardbAnalyticsConnectorErrorKind kind;
};

// Frees an error the library returned and throws it as a
// `ConnectorException` naming `action`; does nothing for a null error.
void Check(const ScalardbAnalyticsConnectorApi &api, ScalardbAnalyticsConnectorError *error, const char *action);

// Describes an error for an exception message: its kind, as the contract
// names it, and its message. The Data Source's own message is left out
// because it may contain user data.
std::string Describe(const ScalardbAnalyticsConnectorError &error);

} // namespace scalardb_analytics_duckdb
