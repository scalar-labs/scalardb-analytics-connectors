// Applying a planned result: checking its remote output and lowering its
// local plan to DuckDB expressions the scan evaluates over the remote rows.
#pragma once

#include <string>
#include <vector>

#include "duckdb/planner/expression.hpp"
#include "substrait/plan.pb.h"
#include "types.hpp"

namespace scalardb_analytics_duckdb {

// The local work of an adopted result, over the remote output's columns.
struct LocalProgram {
	// Keeps only the rows for which it is true; null when no row is removed.
	duckdb::unique_ptr<duckdb::Expression> filter;
	// The output columns, in the order the offered plan requested them.
	duckdb::vector<duckdb::unique_ptr<duckdb::Expression>> projection;
};

// Derives the output columns of a remote plan.
//
// Returns false when the plan is outside the shapes the adapter reads, which
// rejects the result; throws when it breaks the contract.
bool RemoteOutput(const substrait::Plan &remote_plan, std::vector<Field> &output);

// Lowers a local plan over `remote` into a program producing `requested`.
//
// Returns false, with the reason, when any part cannot be lowered; the result
// is then rejected. Throws when the local plan breaks the contract: its
// operators are the offered ones left local, over a RemoteResult that is the
// remote output, producing the requested columns.
bool LowerLocalPlan(duckdb::ClientContext &context, const substrait::Plan &local_plan, const std::vector<Field> &remote,
                    const std::vector<Field> &requested, LocalProgram &program, std::string &reason);

// The program of a result without a local plan: its remote output, which the
// contract makes the requested columns; throws when it is not.
void IdentityProgram(const std::vector<Field> &remote, const std::vector<Field> &requested, LocalProgram &program);

} // namespace scalardb_analytics_duckdb
