// Analytics types in the adapter.
//
// The adapter holds an Analytics type as its Substrait encoding from the
// connector contract (Filters and types), because every plan it builds or
// receives carries types that way. This file owns the conversions at its
// boundaries: from connector API metadata, to DuckDB types (the adapter's
// mapping table), and against the Arrow schema of a read.
#pragma once

#include <string>
#include <vector>

#include "duckdb.hpp"
#include "scalardb_analytics_connector.h"
#include "substrait/type.pb.h"

namespace scalardb_analytics_duckdb {

// The extension URN that declares the `logical` type.
extern const char *const LOGICAL_TYPE_URN;

// The anchor of the `logical` type in the plans the adapter builds.
constexpr uint32_t LOGICAL_TYPE_ANCHOR = 1;

// One column: its name and Analytics type with nullability.
//
// `nested_names` are the names of the struct fields inside the type, in the
// depth-first order Substrait's NamedStruct lists them.
struct Field {
	std::string name;
	substrait::Type type;
	std::vector<std::string> nested_names;
};

// Encodes a connector API Analytics type; `logical_anchor` is the plan's
// anchor of the `logical` type.
Field FieldFromMetadata(const std::string &name, const ScalardbAnalyticsConnectorAnalyticsType &type, bool nullable,
                        uint32_t logical_anchor);

// Reads the fields of a NamedStruct.
std::vector<Field> FieldsOf(const substrait::NamedStruct &schema);

// Writes fields as a NamedStruct.
substrait::NamedStruct NamedStructOf(const std::vector<Field> &fields);

// Whether two types are the same Analytics type, ignoring nullability and
// the anchor that declares `logical`.
bool SameType(const substrait::Type &left, const substrait::Type &right);

// Whether a type is nullable.
bool IsNullable(const substrait::Type &type);

// The name of an extension-encoded type, such as `uuid`, or empty.
std::string LogicalName(const substrait::Type &type);

// The DuckDB type of an Analytics type: the adapter's mapping table.
duckdb::LogicalType DuckDBType(const Field &field);

// The largest decimal precision of Substrait and of DuckDB. A wider Analytics
// decimal has no DuckDB type.
constexpr uint8_t DUCKDB_DECIMAL_PRECISION = 38;

// The Arrow format of a decimal in its Analytics storage, the narrowest Arrow
// decimal that holds the precision, such as `d:10,2,64`.
std::string DecimalFormat(int precision, int scale);

// Checks one field of a read's Arrow schema against its planned Analytics
// type: name, storage type without dictionary encoding, extension name,
// nullability, and struct fields.
void CheckArrowField(const ArrowSchema &schema, const Field &field);

} // namespace scalardb_analytics_duckdb
