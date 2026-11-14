// The scan of a table of an attached Data Source.
//
// DuckDB's optimizer hands the scan its filters through
// `pushdown_complex_filter`, which is called for every scan even without
// filters. There the scan offers its region to the Connector and adopts at
// most one planned result: its remote execution, the local program lowered
// from its local plan, and the filters it covers, which leave DuckDB's
// residual filters in the same step. A result that cannot be adopted changes
// nothing, and a narrower request is offered instead.
//
// Execution reads the adopted remote execution as Arrow, converts it to
// DuckDB types, and evaluates the local program over it.
#pragma once

#include <memory>

#include "catalog.hpp"
#include "duckdb/function/table_function.hpp"

namespace scalardb_analytics_duckdb {

// The scan function every table returns.
duckdb::TableFunction ScanFunction();

// The bind data of a scan of one table.
duckdb::unique_ptr<duckdb::FunctionData> ScanBindData(std::shared_ptr<DataSource> data_source,
                                                      std::shared_ptr<const TableShape> shape,
                                                      duckdb::TableCatalogEntry &table);

} // namespace scalardb_analytics_duckdb
