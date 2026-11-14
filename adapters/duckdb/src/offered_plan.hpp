// Offered plans: one native scan region as a complete Substrait request.
//
// The region is the scan's base read, the filters DuckDB pushed into it that
// Substrait can express, and the columns the query references, in the
// contract's `RelRoot -> ProjectRel -> FilterRel? -> ReadRel` shape.
#pragma once

#include <string>
#include <vector>

#include "catalog.hpp"
#include "duckdb/planner/expression.hpp"
#include "duckdb/planner/operator/logical_get.hpp"
#include "substrait/plan.pb.h"

namespace scalardb_analytics_duckdb {

// The URIs of the profile's function extensions.
extern const char *const COMPARISON_FUNCTIONS_URI;
extern const char *const BOOLEAN_FUNCTIONS_URI;

// Translates one filter DuckDB pushed into the scan to a Substrait condition
// over the base schema. Returns false when Substrait cannot express its
// meaning, in which case the filter stays in DuckDB.
bool TranslateFilter(const duckdb::Expression &filter, const duckdb::LogicalGet &get, const TableShape &shape,
                     substrait::Expression &condition);

// Builds the offered plan that reads `columns`, positions in the base schema,
// of the rows satisfying every condition.
substrait::Plan OfferedPlan(const TableShape &shape, const std::vector<duckdb::idx_t> &columns,
                            const std::vector<substrait::Expression> &conditions);

} // namespace scalardb_analytics_duckdb
