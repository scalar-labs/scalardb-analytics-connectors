// The check of a read's Arrow schema against its planned Analytics types.
//
// The library builds a read's schema from the remote output, so no Connector
// can make it differ; these schemas stand for a C ABI producer that does.
// Each one is valid Arrow that DuckDB would read as another type.
#include "catch2/catch_test_macros.hpp"
#include "catch2/matchers/catch_matchers_string.hpp"
#include "types.hpp"

using Catch::Matchers::ContainsSubstring;
using scalardb_analytics_duckdb::CheckArrowField;
using scalardb_analytics_duckdb::Field;
using scalardb_analytics_duckdb::FieldFromMetadata;
using scalardb_analytics_duckdb::LOGICAL_TYPE_ANCHOR;

namespace {

ArrowSchema Schema(const char *format, const char *name, int64_t n_children = 0, ArrowSchema **children = nullptr) {
	ArrowSchema schema {};
	schema.format = format;
	schema.name = name;
	schema.flags = ARROW_FLAG_NULLABLE;
	schema.n_children = n_children;
	schema.children = children;
	return schema;
}

std::string Failure(const ArrowSchema &schema, const Field &field) {
	try {
		CheckArrowField(schema, field);
	} catch (const std::exception &error) {
		return error.what();
	}
	return "";
}

} // namespace

TEST_CASE("a dictionary-encoded column is not read as its planned type") {
	ScalardbAnalyticsConnectorAnalyticsType int_type {ScalardbAnalyticsConnectorAnalyticsTypeKind_INT};
	auto field = FieldFromMetadata("v", int_type, true, LOGICAL_TYPE_ANCHOR);
	// Int32 indices into Int64 values.
	auto values = Schema("l", "");
	auto schema = Schema("i", "v");
	CHECK(Failure(schema, field).empty());
	schema.dictionary = &values;
	CHECK_THAT(Failure(schema, field), ContainsSubstring("read column v with dictionary encoding other than planned"));
}

TEST_CASE("a struct column with a field more than planned is not read") {
	ScalardbAnalyticsConnectorAnalyticsType big_int {ScalardbAnalyticsConnectorAnalyticsTypeKind_BIG_INT};
	ScalardbAnalyticsConnectorAnalyticsField planned {{"f", 1}, &big_int, ScalardbAnalyticsConnectorBool_TRUE};
	ScalardbAnalyticsConnectorAnalyticsType struct_type {ScalardbAnalyticsConnectorAnalyticsTypeKind_STRUCT};
	struct_type.fields = &planned;
	struct_type.field_count = 1;
	auto field = FieldFromMetadata("s", struct_type, true, LOGICAL_TYPE_ANCHOR);

	auto f = Schema("l", "f");
	auto g = Schema("l", "g");
	ArrowSchema *children[] = {&f, &g};
	CHECK(Failure(Schema("+s", "s", 1, children), field).empty());
	CHECK_THAT(Failure(Schema("+s", "s", 2, children), field),
	           ContainsSubstring("read column s with 2 fields other than planned"));
}
