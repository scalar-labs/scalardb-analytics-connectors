// Planning and adoption at DuckDB's native planner boundary, against the test
// Connector, whose binding selects how it plans and reads. The test Connector
// moves no work to its Data Source: every planned result reads the base table
// remotely and keeps the offered filter and projection in a local plan.
#include "catch2/catch_test_macros.hpp"
#include "catch2/matchers/catch_matchers_string.hpp"
#include "support.hpp"

using Catch::Matchers::ContainsSubstring;
using test_support::Database;

namespace {

// Whether DuckDB still applies a filter above the scan.
bool FilterAboveScan(const std::string &plan) {
	return plan.find("FILTER") != std::string::npos;
}

} // namespace

TEST_CASE("fully-local fallback runs the local filter and projection over the remote rows") {
	Database database;
	test_support::AttachFixture(database, "p", {{{"batches", "3"}, test_support::ValueColumn()}});
	auto query = "SELECT value FROM p.batches.\"3\" WHERE value >= 1 ORDER BY value";

	auto plan = database.Explain(query);
	CHECK_FALSE(FilterAboveScan(plan));
	CHECK_THAT(plan, ContainsSubstring("Local Filter: yes"));
	CHECK(database.Rows(query) == std::vector<std::vector<std::string>> {{"1"}, {"2"}});
}

TEST_CASE("a scan that reads no column counts rows, and a table has no rowid") {
	Database database;
	test_support::AttachFixture(database, "p", {{{"batches", "3"}, test_support::ValueColumn()}});
	CHECK(database.Rows("SELECT count(*) FROM p.batches.\"3\"") == std::vector<std::vector<std::string>> {{"3"}});
	CHECK(database.Rows("SELECT count(*) FROM p.batches.\"3\" WHERE value > 0") ==
	      std::vector<std::vector<std::string>> {{"2"}});
	CHECK(database.Rows("SELECT 1 FROM p.batches.\"3\"") ==
	      std::vector<std::vector<std::string>> {{"1"}, {"1"}, {"1"}});
	// A Data Source's rows have no identifier the scan could return.
	CHECK_THAT(database.Error("SELECT rowid FROM p.batches.\"3\""), ContainsSubstring("rowid"));
}

TEST_CASE("schema and table names resolve as DuckDB identifiers do") {
	Database database;
	test_support::AttachFixture(database, "p", {{{"batches", "2"}, test_support::ValueColumn()}});
	CHECK(database.Rows("SELECT VALUE FROM P.BATCHES.\"2\" ORDER BY 1") ==
	      std::vector<std::vector<std::string>> {{"0"}, {"1"}});
}

TEST_CASE("read, filter, and projection are offered as one region") {
	Database database;
	test_support::AttachFixture(database, "p", {{{"batches", "5"}, test_support::ValueColumn()}});
	// Both filters and the projection belong to one request: the adopted
	// result covers every filter, so none is left above the scan. Each
	// filter removes rows the other keeps, so neither can go missing.
	auto query = "SELECT value + 1 FROM p.batches.\"5\" WHERE value >= 1 AND value IN (0, 1, 3)";
	CHECK_FALSE(FilterAboveScan(database.Explain(query)));
	CHECK(database.Rows(query + std::string(" ORDER BY 1")) == std::vector<std::vector<std::string>> {{"2"}, {"4"}});
}

TEST_CASE("an unlowerable local plan rejects only that proposal and a narrower request is adopted") {
	Database database;
	test_support::AttachFixture(database, "p", {{{"tests", "unlowerable"}, test_support::ValueColumn()}});
	auto query = "SELECT value FROM p.tests.unlowerable WHERE value = 1";

	// The request with the filter returns a local plan the adapter cannot
	// lower; the filter stays in DuckDB and the read alone is adopted.
	auto plan = database.Explain(query);
	CHECK(FilterAboveScan(plan));
	CHECK_THAT(plan, ContainsSubstring("Local Filter: no"));
	CHECK(database.Rows(query) == std::vector<std::vector<std::string>> {{"1"}});
}

TEST_CASE("a planning error leaves the bind state and DuckDB's filters unchanged") {
	Database database;
	test_support::AttachFixture(database, "p",
	                            {{{"tests", "break-connection-planning"}, test_support::ValueColumn()}});
	// Several filters are translated before planning fails; none of them
	// leaves DuckDB, and no result is recorded in the scan.
	auto query = "SELECT value FROM p.tests.\"break-connection-planning\" WHERE value > 1 AND value < 5";
	auto plan = database.Explain(query);
	CHECK(FilterAboveScan(plan));
	CHECK_THAT(plan, !ContainsSubstring("Local Plan"));
	CHECK_THAT(database.Error(query), ContainsSubstring("the test connection broke while planning"));
}

TEST_CASE("an unsupported result leaves DuckDB's plan unchanged") {
	Database database;
	test_support::AttachFixture(database, "p", {{{"tests", "unknown"}, test_support::ValueColumn()}});
	auto query = "SELECT value FROM p.tests.unknown WHERE value > 1";
	auto plan = database.Explain(query);
	CHECK(FilterAboveScan(plan));
	CHECK_THAT(plan, !ContainsSubstring("Local Plan"));
	CHECK_THAT(database.Error(query), ContainsSubstring("did not plan the scan"));
}

TEST_CASE("execution reads the adopted remote execution") {
	Database database;
	test_support::AttachFixture(database, "p",
	                            {{{"batches", "5000"}, test_support::ValueColumn()},
	                             {{"tests", "fail"}, test_support::ValueColumn()}});
	// Many batches cross DuckDB's vector size and are all read.
	CHECK(database.Rows("SELECT count(*), sum(value) FROM p.batches.\"5000\"") ==
	      std::vector<std::vector<std::string>> {{"5000", "12497500"}});
	// A failing read surfaces the Connector's error.
	CHECK_THAT(database.Error("SELECT value FROM p.tests.fail"), ContainsSubstring("the test source failed"));
}

TEST_CASE("a scan the optimizer never offers reads the columns DuckDB asks for") {
	Database database;
	test_support::AttachFixture(database, "p", {{{"batches", "3"}, test_support::ValueColumn()}});
	// Without filter pushdown DuckDB never calls the pushdown callback, so the
	// read is planned when the scan starts and the filter stays in DuckDB.
	database.Rows("SET disabled_optimizers = 'filter_pushdown'");
	auto query = "SELECT value FROM p.batches.\"3\" WHERE value >= 1 ORDER BY value";
	auto plan = database.Explain(query);
	CHECK(FilterAboveScan(plan));
	CHECK_THAT(plan, !ContainsSubstring("Local Plan"));
	CHECK(database.Rows(query) == std::vector<std::vector<std::string>> {{"1"}, {"2"}});
}

TEST_CASE("a later pushdown callback leaves its filters in DuckDB") {
	Database database;
	test_support::AttachFixture(database, "p", {{{"batches", "5"}, test_support::ValueColumn()}});
	// The filter pushdown adopts the CTE's read with its own filter, which
	// leaves DuckDB; the CTE filter pusher then calls back with the outer
	// conditions. Planning again would replace the adopted region with one
	// that lacks `value <> 2`, so those conditions stay in DuckDB instead.
	auto query = "WITH c AS MATERIALIZED (SELECT value FROM p.batches.\"5\" WHERE value <> 2) "
	             "SELECT value FROM c WHERE value >= 2 UNION ALL SELECT value FROM c WHERE value < 1 ORDER BY value";
	CHECK(database.Rows(query) == std::vector<std::vector<std::string>> {{"0"}, {"3"}, {"4"}});
}

TEST_CASE("a read stops at the read timeout") {
	Database database;
	test_support::AttachFixture(database, "p", {{{"tests", "block"}, test_support::ValueColumn()}});
	// The test Connector's read waits until it is stopped.
	database.Rows("SET scalardb_analytics_read_timeout = 100");
	CHECK_THAT(database.Error("SELECT value FROM p.tests.block"), ContainsSubstring("read failed: timeout: "));
}

TEST_CASE("a table of a Data Source is a catalog table that DuckDB does not store") {
	Database database;
	test_support::AttachFixture(database, "p", {{{"batches", "3"}, test_support::ValueColumn()}});
	// ANALYZE reads the table and then fails, since DuckDB keeps statistics
	// only for its own tables.
	CHECK_THAT(database.Error("ANALYZE p.batches.\"3\""), ContainsSubstring("only implemented for DuckDB tables"));
	CHECK_THAT(database.Error("DELETE FROM p.batches.\"3\""), ContainsSubstring("read-only"));
	CHECK_THAT(database.Error("UPDATE p.batches.\"3\" SET value = 1"), ContainsSubstring("read-only"));
	auto merge = database.Error("MERGE INTO p.batches.\"3\" t USING (SELECT 1 AS value) s ON t.value = s.value "
	                            "WHEN MATCHED THEN DELETE");
	INFO(merge);
	CHECK_THAT(merge, !ContainsSubstring("INTERNAL"));
	// The database is still usable after the failed writes.
	CHECK(database.Rows("SELECT count(*) FROM p.batches.\"3\"") == std::vector<std::vector<std::string>> {{"3"}});
}

TEST_CASE("a name that matches two names only ignoring case is ambiguous") {
	Database database;
	test_support::AttachFixture(database, "p",
	                            {{{"Ns", "Ab"}, test_support::ValueColumn()},
	                             {{"Ns", "aB"}, test_support::ValueColumn()},
	                             {{"nS", "x"}, test_support::ValueColumn()}});
	CHECK(database.Rows("DESCRIBE p.Ns.Ab")[0][0] == "value");
	CHECK_THAT(database.Error("DESCRIBE p.Ns.ab"), ContainsSubstring("\"ab\" is ambiguous"));
	CHECK_THAT(database.Error("DESCRIBE p.ns.x"), ContainsSubstring("\"ns\" is ambiguous"));
}

TEST_CASE("values of every Analytics type are read as their mapped DuckDB values") {
	using test_support::FixtureColumn;
	struct Case {
		FixtureColumn column;
		std::string type;
		std::string value;
	};
	// The test Connector reads a fixed value of each column's type, then a
	// row of nulls; its `values` table has these columns, which the schema
	// check compares. The value is the DuckDB text of the mapped value.
	std::vector<Case> cases = {
	    {{"b", ScalardbAnalyticsConnectorAnalyticsTypeKind_BOOLEAN, true}, "BOOLEAN", "true"},
	    {{"i8", ScalardbAnalyticsConnectorAnalyticsTypeKind_BYTE, true}, "TINYINT", "7"},
	    {{"i16", ScalardbAnalyticsConnectorAnalyticsTypeKind_SMALL_INT, true}, "SMALLINT", "7"},
	    {{"i32", ScalardbAnalyticsConnectorAnalyticsTypeKind_INT, true}, "INTEGER", "42"},
	    {{"i64", ScalardbAnalyticsConnectorAnalyticsTypeKind_BIG_INT, true}, "BIGINT", "42"},
	    {{"f32", ScalardbAnalyticsConnectorAnalyticsTypeKind_FLOAT, true}, "FLOAT", "0.5"},
	    {{"f64", ScalardbAnalyticsConnectorAnalyticsTypeKind_DOUBLE, true}, "DOUBLE", "2.5"},
	    {{"dec", ScalardbAnalyticsConnectorAnalyticsTypeKind_DECIMAL, true}, "DECIMAL(10,2)", "10.50"},
	    {{"neg", ScalardbAnalyticsConnectorAnalyticsTypeKind_DECIMAL, true, -2}, "DOUBLE", "105000.0"},
	    {{"txt", ScalardbAnalyticsConnectorAnalyticsTypeKind_TEXT, true}, "VARCHAR", "text"},
	    {{"blb", ScalardbAnalyticsConnectorAnalyticsTypeKind_BLOB, true}, "BLOB", "\\x01\\x02"},
	    {{"d", ScalardbAnalyticsConnectorAnalyticsTypeKind_DATE, true}, "DATE", "2026-01-02"},
	    {{"t", ScalardbAnalyticsConnectorAnalyticsTypeKind_TIME, true}, "TIME", "10:20:30.123456"},
	    {{"ts", ScalardbAnalyticsConnectorAnalyticsTypeKind_TIMESTAMP, true}, "TIMESTAMP", "2026-01-02 10:20:30"},
	    {{"tstz", ScalardbAnalyticsConnectorAnalyticsTypeKind_TIMESTAMP_TZ, true}, "TIMESTAMP WITH TIME ZONE", "2026-01-02 10:20:30+00"},
	    {{"dur", ScalardbAnalyticsConnectorAnalyticsTypeKind_DURATION, true}, "INTERVAL", "00:00:03"},
	    {{"iv", ScalardbAnalyticsConnectorAnalyticsTypeKind_INTERVAL, true}, "INTERVAL", "1 month 2 days 00:00:03"},
	    {{"u", ScalardbAnalyticsConnectorAnalyticsTypeKind_UUID, true}, "UUID", "6f1e1c2a-0000-4000-8000-000000000001"},
	    {{"j", ScalardbAnalyticsConnectorAnalyticsTypeKind_JSON, true}, "JSON", "{\"a\":1}"},
	    {{"y", ScalardbAnalyticsConnectorAnalyticsTypeKind_YEAR, true}, "SMALLINT", "2026"},
	    {{"tz", ScalardbAnalyticsConnectorAnalyticsTypeKind_TIME_WITH_TIME_ZONE, true}, "VARCHAR", "10:00:00+09"},
	    {{"e", ScalardbAnalyticsConnectorAnalyticsTypeKind_ENUM, true}, "ENUM('pending', 'done')", "pending"},
	    {{"arr", ScalardbAnalyticsConnectorAnalyticsTypeKind_ARRAY, true}, "VARCHAR[]", "[text]"},
	    {{"m", ScalardbAnalyticsConnectorAnalyticsTypeKind_MAP, true}, "MAP(VARCHAR, BIGINT)", "{text=42}"},
	    {{"st", ScalardbAnalyticsConnectorAnalyticsTypeKind_STRUCT, true}, "STRUCT(f BIGINT)", "{'f': 42}"},
	};
	std::vector<FixtureColumn> columns;
	for (auto &item : cases) {
		columns.push_back(item.column);
	}
	Database database;
	test_support::AttachFixture(database, "p", {{{"tests", "values"}, columns}});
	database.Rows("SET TimeZone = 'UTC'");
	for (auto &item : cases) {
		auto &name = item.column.name;
		auto query = "SELECT typeof(" + name + "), " + name + "::VARCHAR FROM p.tests.\"values\"";
		INFO(query);
		CHECK(database.Rows(query) ==
		      std::vector<std::vector<std::string>> {{item.type, item.value}, {item.type, "NULL"}});
	}
}

TEST_CASE("a stale registration has the table described again for the next query") {
	Database database;
	// The table is first described with an INTEGER `value`, which the test
	// Connector's schema check finds stale, and then as the source has it.
	test_support::AttachFixture(database, "p",
	                            {{{"batches", "2"}, {{"value", ScalardbAnalyticsConnectorAnalyticsTypeKind_INT, false}}},
	                             {{"batches", "2"}, test_support::ValueColumn()}});
	auto query = "SELECT value FROM p.batches.\"2\" ORDER BY value";
	auto error = database.Error(query);
	CHECK_THAT(error, ContainsSubstring("stale registration: "));
	CHECK_THAT(error, ContainsSubstring("the table is described again when a query next uses it"));
	CHECK(database.Rows(query) == std::vector<std::vector<std::string>> {{"0"}, {"1"}});
}

TEST_CASE("one batch larger than DuckDB's vector size is read in slices") {
	Database database;
	test_support::AttachFixture(database, "p", {{{"rows", "5000"}, test_support::ValueColumn()}});
	CHECK(database.Rows("SELECT count(*), sum(value) FROM p.rows.\"5000\"") ==
	      std::vector<std::vector<std::string>> {{"5000", "12497500"}});
	// The local filter runs over each slice, including rows on both sides of
	// the first slice boundary at 2048.
	CHECK(database.Rows("SELECT value FROM p.rows.\"5000\" WHERE value BETWEEN 2047 AND 2049 ORDER BY value") ==
	      std::vector<std::vector<std::string>> {{"2047"}, {"2048"}, {"2049"}});
}

TEST_CASE("every offered plan shape passes the library's validation") {
	using test_support::FixtureColumn;
	std::vector<FixtureColumn> columns = {
	    {"b", ScalardbAnalyticsConnectorAnalyticsTypeKind_BOOLEAN, true},
	    {"i8", ScalardbAnalyticsConnectorAnalyticsTypeKind_BYTE, true},
	    {"i16", ScalardbAnalyticsConnectorAnalyticsTypeKind_SMALL_INT, true},
	    {"i32", ScalardbAnalyticsConnectorAnalyticsTypeKind_INT, true},
	    {"i64", ScalardbAnalyticsConnectorAnalyticsTypeKind_BIG_INT, false},
	    {"f32", ScalardbAnalyticsConnectorAnalyticsTypeKind_FLOAT, true},
	    {"f64", ScalardbAnalyticsConnectorAnalyticsTypeKind_DOUBLE, true},
	    {"dec", ScalardbAnalyticsConnectorAnalyticsTypeKind_DECIMAL, true},
	    {"txt", ScalardbAnalyticsConnectorAnalyticsTypeKind_TEXT, true},
	    {"blb", ScalardbAnalyticsConnectorAnalyticsTypeKind_BLOB, true},
	    {"d", ScalardbAnalyticsConnectorAnalyticsTypeKind_DATE, true},
	    {"t", ScalardbAnalyticsConnectorAnalyticsTypeKind_TIME, true},
	    {"ts", ScalardbAnalyticsConnectorAnalyticsTypeKind_TIMESTAMP, true},
	    {"tstz", ScalardbAnalyticsConnectorAnalyticsTypeKind_TIMESTAMP_TZ, true},
	    {"dur", ScalardbAnalyticsConnectorAnalyticsTypeKind_DURATION, true},
	    {"iv", ScalardbAnalyticsConnectorAnalyticsTypeKind_INTERVAL, true},
	    {"u", ScalardbAnalyticsConnectorAnalyticsTypeKind_UUID, true},
	    {"j", ScalardbAnalyticsConnectorAnalyticsTypeKind_JSON, true},
	    {"y", ScalardbAnalyticsConnectorAnalyticsTypeKind_YEAR, true},
	    {"tz", ScalardbAnalyticsConnectorAnalyticsTypeKind_TIME_WITH_TIME_ZONE, true},
	    {"e", ScalardbAnalyticsConnectorAnalyticsTypeKind_ENUM, true},
	    {"neg", ScalardbAnalyticsConnectorAnalyticsTypeKind_DECIMAL, true, -2},
	    {"arr", ScalardbAnalyticsConnectorAnalyticsTypeKind_ARRAY, true},
	    {"m", ScalardbAnalyticsConnectorAnalyticsTypeKind_MAP, true},
	    {"st", ScalardbAnalyticsConnectorAnalyticsTypeKind_STRUCT, true},
	};
	Database database;
	test_support::AttachFixture(database, "p", {{{"tests", "shapes"}, columns}});
	// One condition per query, so each row states whether that condition is
	// offered. The test Connector does not know the table, so a valid offered
	// plan is unsupported; an invalid one would be rejected as an invalid
	// request, and a condition not offered leaves no filtered request.
	struct Case {
		std::string filter;
		bool offered;
	};
	std::vector<Case> cases = {
	    {"b = true", true},
	    {"i8 < 3", true},
	    {"i16 <= 3", true},
	    {"i32 > 3", true},
	    {"i64 >= 3", true},
	    {"f32 <> 1.5", true},
	    {"f64 = 2.5", true},
	    {"f32 <> 1.5 OR f64 = 2.5", true},
	    {"dec BETWEEN 1.25 AND 9.75", true},
	    {"txt = 'a'", true},
	    {"blb = '\\x01'::BLOB", true},
	    {"d = DATE '2026-01-01'", true},
	    {"t < TIME '10:00:00'", true},
	    {"ts > TIMESTAMP '2026-01-01 00:00:00'", true},
	    {"tstz < TIMESTAMPTZ '2026-01-01 00:00:00+00'", true},
	    {"u = '6f1e1c2a-0000-4000-8000-000000000000'", true},
	    {"j = '{}'::JSON", true},
	    {"y = 2026", true},
	    {"tz = '10:00:00+09'", true},
	    // DuckDB binds these as CAST(e AS VARCHAR) compared with a string, which
	    // is Enum equality when the string is a member.
	    {"e = 'done'", true},
	    {"'done' <> e", true},
	    {"e IN ('pending', 'done')", true},
	    {"e = 'unknown'", false},
	    {"e < 'done'", false},
	    {"NOT (i64 IS NULL)", true},
	    {"txt IS NOT NULL", true},
	    {"dur IS NULL", true},
	    {"iv IS NULL", true},
	    {"neg IS NULL", true},
	    {"arr IS NULL", true},
	    {"m IS NOT NULL", true},
	    {"st IS NULL", true},
	};
	for (auto &item : cases) {
		auto query = "SELECT * FROM p.tests.shapes WHERE " + item.filter;
		INFO(query);
		auto error = database.Error(query);
		CHECK_THAT(error, ContainsSubstring("the test connector does not know this table"));
		auto filtered = error.find("with its filters, the Connector did not plan the scan") != std::string::npos;
		CHECK(filtered == item.offered);
	}
	CHECK_THAT(database.Error("SELECT count(*) FROM p.tests.shapes"), ContainsSubstring("does not know this table"));
}

TEST_CASE("a time of 24:00:00 stays in DuckDB while the other filters are offered") {
	Database database;
	test_support::AttachFixture(database, "p",
	                            {{{"tests", "shapes"},
	                              {{"t", ScalardbAnalyticsConnectorAnalyticsTypeKind_TIME, true},
	                               {"b", ScalardbAnalyticsConnectorAnalyticsTypeKind_BOOLEAN, true}}}});
	// A Substrait time cannot hold 24:00:00; only that condition is left out
	// of the request, which still reaches the Connector's planner.
	CHECK_THAT(database.Error("SELECT * FROM p.tests.shapes WHERE t <= TIME '24:00:00' AND b = true"),
	           ContainsSubstring("with its filters, the Connector did not plan the scan: "
	                             "the test connector does not know this table"));
}
