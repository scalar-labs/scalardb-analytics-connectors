// The loadable extension in a process whose connector API library another
// caller already initialized, as an application that owns the library does
// when it runs DuckDB through a binding that can only load extensions.
//
// The test process also carries the adapter and its Substrait messages: the
// extension keeps its own protobuf, so loading it registers its messages
// only there.
#include <string>

#include "catch2/catch_test_macros.hpp"
#include "duckdb.hpp"
#include "support.hpp"

namespace {

// Runs a query, failing the test with its error.
duckdb::unique_ptr<duckdb::MaterializedQueryResult> Run(duckdb::Connection &connection, const std::string &sql) {
	auto result = connection.Query(sql);
	INFO(sql);
	INFO((result->HasError() ? result->GetError() : std::string()));
	REQUIRE_FALSE(result->HasError());
	return result;
}

} // namespace

TEST_CASE("the loadable extension uses a library another caller initialized") {
	// The test process owns the library.
	test_support::Api();

	duckdb::DBConfig config;
	config.SetOptionByName("allow_unsigned_extensions", duckdb::Value::BOOLEAN(true));
	duckdb::DuckDB database(nullptr, &config);
	duckdb::Connection connection(database);
	Run(connection, "LOAD '" SCALARDB_ANALYTICS_EXTENSION_FILE "'");

	// The extension's attach type opens Data Sources on the library the
	// process owns.
	Run(connection, "ATTACH 'test' AS t (TYPE scalardb_analytics_connectors, label 'duckdb-extension')");
	auto tables = Run(connection, "SELECT table_name FROM information_schema.tables WHERE table_catalog = 't' "
	                              "AND table_schema = 'test.public' ORDER BY table_name");
	REQUIRE(tables->RowCount() == 2);
	CHECK(tables->GetValue(0, 0).ToString() == "order_view");
	CHECK(tables->GetValue(0, 1).ToString() == "orders");
}
