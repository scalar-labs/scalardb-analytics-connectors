// Attaching Data Sources and browsing their catalogs, against the test
// Connector's fixed catalog.
#include <cstdio>
#include <fstream>

#include "catch2/catch_test_macros.hpp"
#include "catch2/matchers/catch_matchers_string.hpp"
#include "support.hpp"

using Catch::Matchers::ContainsSubstring;
using test_support::Database;

TEST_CASE("an attached Data Source is a catalog of its namespaces and tables") {
	Database database;
	database.Rows("ATTACH 'test' AS t (TYPE scalardb_analytics, label '" + test_support::UniqueLabel() + "')");

	// Readable namespaces become schemas named by their name path.
	CHECK(database.Rows("SELECT schema_name FROM information_schema.schemata WHERE catalog_name = 't' "
	                    "ORDER BY schema_name") ==
	      std::vector<std::vector<std::string>> {{"test.public"}, {"test.sales"}});
	CHECK(database.Rows("SELECT table_name FROM information_schema.tables WHERE table_catalog = 't' "
	                    "AND table_schema = 'test.public' ORDER BY table_name") ==
	      std::vector<std::vector<std::string>> {{"order_view"}, {"orders"}});

	// Columns with an Analytics type, mapped to DuckDB types with the
	// nullability the Connector reported; the unsupported column `area` is
	// not part of the table.
	CHECK(database.Rows("SELECT column_name, data_type, is_nullable FROM information_schema.columns "
	                    "WHERE table_catalog = 't' AND table_name = 'orders' ORDER BY ordinal_position") ==
	      std::vector<std::vector<std::string>> {{"id", "BIGINT", "NO"},
	                                             {"amount", "DECIMAL(10,2)", "YES"},
	                                             {"reference", "UUID", "YES"},
	                                             {"tags", "VARCHAR[]", "YES"}});
}

TEST_CASE("namespaces whose dotted names would coincide are distinct schemas") {
	Database database;
	test_support::AttachFixture(database, "p",
	                            {{{"a.b", "x"}, test_support::ValueColumn()},
	                             {{"a", "b", "y"}, test_support::ValueColumn()},
	                             {{"q\"", "z"}, test_support::ValueColumn()}});
	// A name with a dot or a double quote is quoted inside the schema name.
	CHECK(database.Rows("SELECT table_schema, table_name FROM information_schema.tables WHERE table_catalog = 'p' "
	                    "ORDER BY table_name") ==
	      std::vector<std::vector<std::string>> {{"\"a.b\"", "x"}, {"a.b", "y"}, {"\"q\"\"\"", "z"}});
	// Each schema is looked up by its own name.
	CHECK(database.Rows("DESCRIBE p.\"\"\"a.b\"\"\".x")[0][0] == "value");
	CHECK(database.Rows("DESCRIBE p.\"a.b\".y")[0][0] == "value");
}

TEST_CASE("a column without an Analytics type can be named but not read") {
	Database database;
	database.Rows("ATTACH 'test' AS t (TYPE scalardb_analytics, label '" + test_support::UniqueLabel() + "')");
	auto orders = std::string("t.\"test.public\".orders");
	// `area` is a geometry column: naming it, even only in a filter, fails
	// with the reason rather than as an unknown column.
	auto reason = "cannot read column area of orders: geometry has no Analytics type";
	CHECK_THAT(database.Error("SELECT area FROM " + orders), ContainsSubstring(reason));
	CHECK_THAT(database.Error("SELECT id FROM " + orders + " WHERE area IS NULL"), ContainsSubstring(reason));
	// `*` holds only the readable columns, and the table's comment names the
	// others.
	CHECK(database.Rows("SELECT column_name FROM (DESCRIBE " + orders + ") ORDER BY ALL") ==
	      std::vector<std::vector<std::string>> {{"amount"}, {"id"}, {"reference"}, {"tags"}});
	CHECK(database.Rows("SELECT comment FROM duckdb_tables() WHERE database_name = 't' AND table_name = 'orders'") ==
	      std::vector<std::vector<std::string>> {
	          {"Columns without an Analytics type: area (geometry has no Analytics type)"}});
}

TEST_CASE("a catalog request stops at the metadata timeout set when attaching") {
	Database database;
	database.Rows("SET scalardb_analytics_metadata_timeout = 100");
	database.Rows("ATTACH 'test' AS t (TYPE scalardb_analytics, label '" + test_support::UniqueLabel() + "')");
	// The test Connector's description of a table named `block` waits until
	// it is stopped.
	CHECK_THAT(database.Error("SELECT * FROM t.\"test.public\".block"),
	           ContainsSubstring("could not read table metadata: timeout: "));
}

TEST_CASE("a name not listed is described by its own request") {
	Database database;
	database.Rows("ATTACH 'test' AS t (TYPE scalardb_analytics, label '" + test_support::UniqueLabel() + "')");
	// The list cannot read the namespace `test.restricted` and does not show
	// it; describing it reports why instead of saying it does not exist.
	CHECK_THAT(database.Error("SELECT * FROM t.\"test.restricted\".x"),
	           ContainsSubstring("could not read the namespace: permission denied for the namespace"));
	// The table `secret` is not listed; its metadata may not be read.
	CHECK_THAT(database.Error("SELECT * FROM t.\"test.public\".secret"),
	           ContainsSubstring("may not read the table metadata: permission denied for the table"));
	// The table `hidden` is not found, and whether it exists is unknown, so
	// it is not reported missing.
	CHECK_THAT(database.Error("SELECT * FROM t.\"test.public\".hidden"),
	           ContainsSubstring("could not find the table: the catalog may hide the table"));
	// A name whose description says it does not exist is missing.
	CHECK_THAT(database.Error("SELECT * FROM t.\"test.nowhere\".x"), ContainsSubstring("does not exist"));
	CHECK_THAT(database.Error("SELECT * FROM t.\"test.public\".nowhere"), ContainsSubstring("does not exist"));
	// The lists still show only what they could read.
	CHECK(database.Rows("SELECT count(*) FROM information_schema.schemata WHERE catalog_name = 't' "
	                    "AND schema_name = 'test.restricted'") == std::vector<std::vector<std::string>> {{"0"}});
}

TEST_CASE("an attached catalog is read-only") {
	Database database;
	database.Rows("ATTACH 'test' AS t (TYPE scalardb_analytics, label '" + test_support::UniqueLabel() + "')");
	CHECK_THAT(database.Error("CREATE TABLE t.\"test.public\".created (id INTEGER)"), ContainsSubstring("read-only"));
	CHECK_THAT(database.Error("INSERT INTO t.\"test.public\".orders (id) VALUES (1)"), ContainsSubstring("read-only"));
	CHECK_THAT(database.Error("CREATE SCHEMA t.created"), ContainsSubstring("read-only"));
}

TEST_CASE("invalid connection options fail the ATTACH") {
	Database database;
	CHECK_THAT(database.Error("ATTACH 'test' AS t (TYPE scalardb_analytics)"), ContainsSubstring("label"));
	CHECK_THAT(database.Error("ATTACH 'nonexistent' AS t (TYPE scalardb_analytics)"),
	           ContainsSubstring("has no Connector nonexistent"));
}

TEST_CASE("connection options have the types the Connector declares") {
	auto label = test_support::UniqueLabel();
	auto attach = [&](const std::string &options) {
		return "ATTACH 'test' AS t (TYPE scalardb_analytics, label '" + label + "'" + options + ")";
	};
	{
		Database database;
		database.Rows(attach(", attempts 3, verbose true"));
		CHECK(database.Rows("SELECT count(*) FROM duckdb_databases() WHERE database_name = 't'") ==
		      std::vector<std::vector<std::string>> {{"1"}});
	}
	struct Case {
		std::string options;
		std::string error;
	};
	// A value of another type is not converted: 1e3 would otherwise reach
	// the Connector as 1000.0 and 0123 as 123.
	std::vector<Case> cases = {
	    {", attempts '3'", "option \"attempts\" of Connector test must be an integer within 64 bits, not VARCHAR"},
	    {", attempts 1e3", "must be an integer within 64 bits, not DOUBLE"},
	    {", attempts 18446744073709551615::UBIGINT", "must be an integer within 64 bits, not UBIGINT"},
	    {", verbose 'true'", "option \"verbose\" of Connector test must be a boolean, not VARCHAR"},
	    {", token 0123", "option \"token\" of Connector test must be a string, not INTEGER"},
	    {", unknown 'x'", "Connector test does not accept option \"unknown\""},
	};
	for (auto &item : cases) {
		Database database;
		INFO(item.options);
		auto error = database.Error(attach(item.options));
		CHECK_THAT(error, ContainsSubstring(item.error));
		CHECK_THAT(error, !ContainsSubstring("0123"));
	}
}

TEST_CASE("a database without external access attaches no Data Source") {
	Database database;
	database.Rows("SET enable_external_access = false");
	CHECK_THAT(database.Error("ATTACH 'test' AS t (TYPE scalardb_analytics, label '" + test_support::UniqueLabel() +
	                          "')"),
	           ContainsSubstring("enable_external_access"));
}

TEST_CASE("the bulk-attach pragma attaches every Data Source of a definitions file") {
	Database database;
	auto path = std::string(std::getenv("TMPDIR") ? std::getenv("TMPDIR") : "/tmp") + "/duckdb-adapter-sources.json";
	{
		std::ofstream file(path);
		file << "[{\"name\": \"first\", \"connector\": \"test\", \"options\": {\"label\": \""
		     << test_support::UniqueLabel() << "\"}},"
		     << " {\"name\": \"second\", \"connector\": \"test\", \"options\": {\"label\": \""
		     << test_support::UniqueLabel()
		     << "\", \"token\": \"it's secret\", \"attempts\": 3, \"verbose\": false}}]";
	}
	database.Rows("CALL enable_logging()");
	database.Rows("PRAGMA scalardb_analytics_attach('" + path + "')");
	std::remove(path.c_str());

	// The connection options, such as the token, never reach the query log.
	CHECK(database.Rows("SELECT count(*) FROM duckdb_logs() WHERE message LIKE '%secret%'") ==
	      std::vector<std::vector<std::string>> {{"0"}});

	// Each Data Source is an ordinary attached catalog.
	CHECK(database.Rows("SELECT database_name, type FROM duckdb_databases() WHERE type = 'scalardb_analytics' "
	                    "ORDER BY database_name") ==
	      std::vector<std::vector<std::string>> {{"first", "scalardb_analytics"}, {"second", "scalardb_analytics"}});
	CHECK(database.Rows("SELECT count(*) FROM information_schema.tables WHERE table_catalog = 'second'") ==
	      std::vector<std::vector<std::string>> {{"3"}});
	database.Rows("DETACH first");
}

namespace {

// Writes a definitions file and returns its path.
std::string DefinitionsFile(const std::string &content) {
	static int next = 0;
	auto path = std::string(std::getenv("TMPDIR") ? std::getenv("TMPDIR") : "/tmp") + "/duckdb-adapter-definitions-" +
	            std::to_string(next++) + ".json";
	std::ofstream(path) << content;
	return path;
}

} // namespace

TEST_CASE("the bulk-attach pragma rejects a malformed definitions file and attaches nothing") {
	auto label = test_support::UniqueLabel();
	auto good = "{\"name\": \"good\", \"connector\": \"test\", \"options\": {\"label\": \"" + label + "\"}}";
	struct Case {
		std::string entry;
		std::string error;
	};
	std::vector<Case> cases = {
	    {"{\"connector\": \"test\", \"options\": {\"label\": \"x\"}}", "with \"name\" and \"connector\""},
	    {"{\"name\": \"\", \"connector\": \"test\"}", "\"name\" must be a non-empty string"},
	    {"{\"name\": null, \"connector\": \"test\"}", "\"name\" must be a non-empty string, not NULL"},
	    {"{\"name\": \"b\", \"connector\": \"test\", \"option\": {\"label\": \"x\"}}", "unknown key \"option\""},
	    {"{\"name\": \"b\", \"connector\": \"test\", \"options\": {\"label\": \"x\", \"token\": null}}",
	     "option \"token\" must be a string, an integer, or a boolean, not NULL"},
	    {"{\"name\": \"b\", \"connector\": \"test\", \"options\": {\"label\": \"x\", \"attempts\": 1.5}}",
	     "option \"attempts\" must be a string, an integer, or a boolean, not DOUBLE"},
	    // Valid JSON types, checked against the Connector's declared types.
	    {"{\"name\": \"b\", \"connector\": \"test\", \"options\": {\"label\": 5432}}",
	     "option \"label\" of Connector test must be a string, not UBIGINT"},
	    {"{\"name\": \"b\", \"connector\": \"test\", \"options\": {\"label\": \"x\", \"attempts\": \"3\"}}",
	     "option \"attempts\" of Connector test must be an integer within 64 bits, not VARCHAR"},
	    {"{\"name\": \"b\", \"connector\": \"test\", \"options\": {\"label\": \"x\", \"TYPE\": \"duckdb\"}}",
	     "option \"TYPE\" is an ATTACH option of DuckDB"},
	    {"{\"name\": \"GOOD\", \"connector\": \"test\", \"options\": {\"label\": \"x\"}}",
	     "already used by another entry"},
	    {"\"pg\"", "an entry must be an object"},
	    // Valid in shape; the Connector rejects the options after the first
	    // entry was attached.
	    {"{\"name\": \"b\", \"connector\": \"test\"}", "label"},
	};
	for (auto &item : cases) {
		Database database;
		auto path = DefinitionsFile("[" + good + ", " + item.entry + "]");
		INFO(item.entry);
		auto error = database.Error("PRAGMA scalardb_analytics_attach('" + path + "')");
		std::remove(path.c_str());
		CHECK_THAT(error, ContainsSubstring(item.error));
		// No value of the file appears in the message.
		CHECK_THAT(error, !ContainsSubstring(label));
		// Nothing stays attached, including the valid first entry.
		CHECK(database.Rows("SELECT count(*) FROM duckdb_databases() WHERE type = 'scalardb_analytics'") ==
		      std::vector<std::vector<std::string>> {{"0"}});
	}
}
