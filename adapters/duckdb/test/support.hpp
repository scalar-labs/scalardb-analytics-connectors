// Shared setup of the adapter's integration tests.
//
// The tests embed DuckDB the way an application does: the test process owns
// the connector API library, initializes it once, and registers the adapter
// on each database through the embedding entry.
#pragma once

#include <memory>
#include <string>
#include <vector>

#include "duckdb.hpp"
#include "scalardb_analytics_duckdb.hpp"

namespace test_support {

// The initialized connector API library.
const ScalardbAnalyticsConnectorApi &Api();

// An in-memory DuckDB database with the adapter registered.
struct Database {
	Database();

	// Runs a query and returns its rows as text, failing the test on error.
	std::vector<std::vector<std::string>> Rows(const std::string &sql);
	// Runs a query and returns its error message, failing the test if it
	// succeeds.
	std::string Error(const std::string &sql);
	// Returns the physical plan EXPLAIN prints.
	std::string Explain(const std::string &sql);

	duckdb::DuckDB database;
	duckdb::Connection connection;
};

// A column of a table a fixture catalog source serves. A Decimal has
// precision 10 and the given scale, an Enum the members `pending` and `done`,
// an Array Text elements, a Map Text keys and BigInt values, and a Struct one
// BigInt field `f`.
struct FixtureColumn {
	std::string name;
	ScalardbAnalyticsConnectorAnalyticsTypeKind kind;
	bool nullable;
	int8_t decimal_scale = 2;
};

// Attaches `alias`: a Data Source of the test Connector whose catalog comes
// from a catalog source that serves the given tables, each as its name path
// with columns, the way a component holding registered metadata does. A path
// given more than once is a table whose shape changes: each description
// returns its next entry, and the last one from then on.
void AttachFixture(Database &database, const std::string &alias,
                   const std::vector<std::pair<scalardb_analytics_duckdb::Names, std::vector<FixtureColumn>>> &tables);

// The single non-null BIGINT column `value` every test Connector read has.
std::vector<FixtureColumn> ValueColumn();

// A label no other test uses, naming the test Connector's probe.
std::string UniqueLabel();

} // namespace test_support
