#include "scan.hpp"

#include <cmath>

#include "connector_api.hpp"
#include "duckdb/common/types/data_chunk.hpp"
#include "duckdb/execution/expression_executor.hpp"
#include "duckdb/function/table/arrow.hpp"
#include "duckdb/planner/expression/bound_reference_expression.hpp"
#include "local_plan.hpp"
#include "offered_plan.hpp"
#include "settings.hpp"

namespace scalardb_analytics_duckdb {

namespace {

// An adopted planned result.
struct Adopted {
	Adopted(const ScalardbAnalyticsConnectorApi &api_p) : api(api_p) {
	}
	~Adopted() {
		api.remote_execution_free(remote_execution);
	}

	const ScalardbAnalyticsConnectorApi &api;
	ScalardbAnalyticsConnectorRemoteExecution *remote_execution = nullptr;
	// The columns the remote execution returns.
	std::vector<Field> remote_output;
	// The local work producing the requested columns from the remote output.
	LocalProgram program;
	// The base columns the program produces, in order.
	std::vector<duckdb::idx_t> columns;
	// Whether a local plan remains, for EXPLAIN.
	bool local_plan = false;
};

// What a stale-registration message adds: how the adapter recovers.
const char *const STALE_RECOVERY = "; the table is described again when a query next uses it";

// Marks the table's shape stale when the Connector reported a stale
// registration, `does not exist` or `schema mismatch`, and returns what the
// message adds.
std::string Stale(const TableShape &shape, ScalardbAnalyticsConnectorErrorKind kind) {
	if (kind != SCALARDB_ANALYTICS_CONNECTOR_ERROR_KIND_DOES_NOT_EXIST &&
	    kind != SCALARDB_ANALYTICS_CONNECTOR_ERROR_KIND_SCHEMA_MISMATCH) {
		return "";
	}
	shape.stale = true;
	return STALE_RECOVERY;
}

struct ScanData : public duckdb::TableFunctionData {
	ScanData(std::shared_ptr<DataSource> data_source_p, std::shared_ptr<const TableShape> shape_p,
	         duckdb::TableCatalogEntry &table_p)
	    : data_source(std::move(data_source_p)), shape(std::move(shape_p)), table(table_p) {
	}

	duckdb::unique_ptr<duckdb::FunctionData> Copy() const override {
		auto copy = duckdb::make_uniq<ScanData>(data_source, shape, table);
		copy->adopted = adopted;
		copy->offered = offered;
		copy->failure = failure;
		return std::move(copy);
	}

	bool Equals(const duckdb::FunctionData &other) const override {
		auto &data = other.Cast<ScanData>();
		return shape == data.shape && adopted == data.adopted && offered == data.offered;
	}

	std::shared_ptr<DataSource> data_source;
	std::shared_ptr<const TableShape> shape;
	// The catalog entry the scan reads, which outlives it.
	duckdb::TableCatalogEntry &table;
	// The adopted result, if any.
	std::shared_ptr<const Adopted> adopted;
	// Whether the scan's region was offered during optimization.
	bool offered = false;
	// Why the last request was not adopted.
	std::string failure;
};

// Plans one request and adopts its result if the adapter can install it.
//
// Returns null, with the reason in `failure`, for an unsupported result, an
// error, or a result that cannot be installed; nothing outside the returned
// value changes in any case. A result that breaks the contract, which only a
// library defect produces, is thrown instead.
std::shared_ptr<const Adopted> Plan(duckdb::ClientContext &context, const ScanData &data,
                                    const std::vector<duckdb::idx_t> &columns,
                                    const std::vector<substrait::Expression> &conditions, std::string &failure) {
	auto &api = data.data_source->api;
	auto &shape = *data.shape;
	std::vector<Field> requested;
	for (auto column : columns) {
		requested.push_back(shape.fields[column]);
	}
	auto bytes = OfferedPlan(shape, columns, conditions).SerializeAsString();
	ScalardbAnalyticsConnectorPushdownResult *result = nullptr;
	try {
		auto lease = data.data_source->Session();
		lease.Check(api.plan_pushdown(lease.session,
		                              {reinterpret_cast<const uint8_t *>(bytes.data()), bytes.size()}, &result),
		            "plan the scan");
	} catch (ConnectorException &error) {
		failure = duckdb::ErrorData(error).RawMessage() + Stale(*data.shape, error.kind);
		return nullptr;
	} catch (std::exception &error) {
		failure = duckdb::ErrorData(error).RawMessage();
		return nullptr;
	}
	std::unique_ptr<ScalardbAnalyticsConnectorPushdownResult, std::function<void(ScalardbAnalyticsConnectorPushdownResult *)>>
	    owned(result, [&api](ScalardbAnalyticsConnectorPushdownResult *r) { api.pushdown_result_free(r); });
	if (!api.pushdown_result_is_planned(result)) {
		failure = "the Connector did not plan the scan: " + Text(api.pushdown_result_unsupported_reason(result));
		return nullptr;
	}
	auto adopted = std::make_shared<Adopted>(api);
	adopted->columns = columns;
	substrait::Plan remote_plan;
	auto remote_bytes = api.pushdown_result_remote_plan(result);
	if (!remote_plan.ParseFromArray(remote_bytes.data, static_cast<int>(remote_bytes.length))) {
		throw duckdb::IOException("ScalarDB Analytics returned a remote plan that cannot be parsed");
	}
	if (!RemoteOutput(remote_plan, adopted->remote_output)) {
		failure = "the remote plan is outside the shapes the adapter reads";
		return nullptr;
	}
	auto local_bytes = api.pushdown_result_local_plan(result);
	if (local_bytes.data) {
		substrait::Plan local_plan;
		if (!local_plan.ParseFromArray(local_bytes.data, static_cast<int>(local_bytes.length))) {
			throw duckdb::IOException("ScalarDB Analytics returned a local plan that cannot be parsed");
		}
		adopted->local_plan = true;
		if (!LowerLocalPlan(context, local_plan, adopted->remote_output, requested, adopted->program, failure)) {
			return nullptr;
		}
	} else {
		IdentityProgram(adopted->remote_output, requested, adopted->program);
	}
	auto error = api.pushdown_result_remote_execution(result, &adopted->remote_execution);
	if (error) {
		failure = Describe(*error);
		api.error_free(error);
		return nullptr;
	}
	return adopted;
}

// The base columns a scan reads, in DuckDB's column order; a scan that
// reads none, such as one for `count(*)`, reads the first.
std::vector<duckdb::idx_t> ReadColumns(const duckdb::vector<duckdb::column_t> &ids) {
	std::vector<duckdb::idx_t> columns;
	for (auto id : ids) {
		if (!duckdb::IsVirtualColumn(id) && std::find(columns.begin(), columns.end(), id) == columns.end()) {
			columns.push_back(id);
		}
	}
	if (columns.empty()) {
		columns.push_back(0);
	}
	return columns;
}

void PushdownComplexFilter(duckdb::ClientContext &context, duckdb::LogicalGet &get, duckdb::FunctionData *bind_data,
                           duckdb::vector<duckdb::unique_ptr<duckdb::Expression>> &filters) {
	auto &data = bind_data->Cast<ScanData>();
	if (data.adopted) {
		// DuckDB may call again with further filters, such as the CTE filter
		// pusher does. The filters the adopted result covers have already
		// left DuckDB, so planning again with only the new ones would lose
		// them; the new ones stay in DuckDB.
		return;
	}
	data.offered = true;
	duckdb::vector<duckdb::column_t> ids;
	for (auto &id : get.GetColumnIds()) {
		ids.push_back(id.GetPrimaryIndex());
	}
	auto columns = ReadColumns(ids);

	// The whole region first: every filter Substrait can express.
	std::vector<substrait::Expression> conditions;
	std::vector<duckdb::idx_t> covered;
	for (duckdb::idx_t i = 0; i < filters.size(); i++) {
		substrait::Expression condition;
		if (TranslateFilter(*filters[i], get, *data.shape, condition)) {
			conditions.push_back(std::move(condition));
			covered.push_back(i);
		}
	}
	std::string failure;
	if (!conditions.empty()) {
		auto adopted = Plan(context, data, columns, conditions, failure);
		if (adopted) {
			// Adoption: the result and the filters it covers change together.
			for (auto i = covered.rbegin(); i != covered.rend(); i++) {
				filters.erase(filters.begin() + static_cast<std::ptrdiff_t>(*i));
			}
			data.adopted = std::move(adopted);
			return;
		}
		failure = "with its filters, " + failure + "; without them, ";
	}
	// Then the read alone, leaving every filter in DuckDB.
	std::string read_failure;
	data.adopted = Plan(context, data, columns, {}, read_failure);
	data.failure = failure + read_failure;
}

// How one output column of the scan is produced.
struct OutputColumn {
	// The position in the local program's output; unused for the empty column.
	duckdb::idx_t position;
	// Whether this is DuckDB's empty column, which a scan that reads no column
	// requests and whose values DuckDB never reads.
	bool empty;
};

struct ScanGlobalState : public duckdb::GlobalTableFunctionState {
	ScanGlobalState(std::shared_ptr<DataSource> data_source_p, std::shared_ptr<const TableShape> shape_p,
	                std::shared_ptr<const Adopted> adopted_p)
	    : data_source(std::move(data_source_p)), shape(std::move(shape_p)), adopted(std::move(adopted_p)),
	      lease(data_source->Session()) {
		stream.release = nullptr;
		schema.release = nullptr;
	}

	~ScanGlobalState() override {
		// The stream and operation end before the session returns.
		if (stream.release) {
			stream.release(&stream);
		}
		if (schema.release) {
			schema.release(&schema);
		}
		data_source->api.operation_close(operation);
		if (failed) {
			lease.Discard();
		}
	}

	duckdb::idx_t MaxThreads() const override {
		return 1;
	}

	// Marks the read failed and describes why its stream failed: the
	// operation's error, or else the stream's message, which the Arrow C
	// stream interface lets be null.
	std::string Failure() {
		failed = true;
		if (auto error = data_source->api.operation_error(operation)) {
			return Describe(*error) + Stale(*shape, error->kind);
		}
		auto message = stream.get_last_error(&stream);
		return message ? message : "the stream reported no detail";
	}

	std::shared_ptr<DataSource> data_source;
	std::shared_ptr<const TableShape> shape;
	std::shared_ptr<const Adopted> adopted;
	DataSource::SessionLease lease;
	ScalardbAnalyticsConnectorOperation *operation = nullptr;
	ArrowArrayStream stream;
	ArrowSchema schema;
	duckdb::ArrowTableSchema arrow_schema;
	// Top-level negative-scale decimals and the power of ten that scales them.
	std::vector<std::pair<duckdb::idx_t, double>> scaled;
	std::vector<OutputColumn> outputs;
	bool done = false;
	// Whether the read failed, which may leave the session unusable.
	bool failed = false;
};

// Reads the Arrow schema at the head of the stream and checks it against the
// planned remote output before DuckDB interprets it.
void ReadSchema(duckdb::ClientContext &context, ScanGlobalState &state) {
	if (state.stream.get_schema(&state.stream, &state.schema) != 0) {
		throw duckdb::IOException("ScalarDB Analytics could not read the schema: %s", state.Failure());
	}
	auto &fields = state.adopted->remote_output;
	if (static_cast<size_t>(state.schema.n_children) != fields.size()) {
		throw duckdb::IOException("ScalarDB Analytics read %d columns where %d were planned",
		                          static_cast<int>(state.schema.n_children), static_cast<int>(fields.size()));
	}
	for (duckdb::idx_t i = 0; i < fields.size(); i++) {
		CheckArrowField(*state.schema.children[i], fields[i]);
	}

	// DuckDB has no negative decimal scale: such a column is read as its
	// unscaled integers and scaled after conversion. Its format is replaced
	// only while DuckDB interprets the schema. The producer frees the format
	// it allocated when the schema is released, so the original is restored
	// on every exit, including an exception from DuckDB.
	std::vector<std::string> replacements;
	replacements.reserve(fields.size());
	struct BorrowedFormats {
		~BorrowedFormats() {
			for (auto &format : formats) {
				format.first->format = format.second;
			}
		}
		std::vector<std::pair<ArrowSchema *, const char *>> formats;
	} borrowed;
	for (duckdb::idx_t i = 0; i < fields.size(); i++) {
		auto &type = fields[i].type;
		if (type.kind_case() == substrait::Type::kDecimal && type.decimal().scale() < 0) {
			auto &child = *state.schema.children[i];
			replacements.push_back(DecimalFormat(type.decimal().precision(), 0));
			borrowed.formats.emplace_back(&child, child.format);
			child.format = replacements.back().c_str();
			state.scaled.emplace_back(i, std::pow(10.0, -type.decimal().scale()));
		}
	}
	duckdb::ArrowTableFunction::PopulateArrowTableSchema(context, state.arrow_schema, state.schema);
}

duckdb::unique_ptr<duckdb::GlobalTableFunctionState> InitGlobal(duckdb::ClientContext &context,
                                                                duckdb::TableFunctionInitInput &input) {
	auto &data = input.bind_data->Cast<ScanData>();
	auto adopted = data.adopted;
	std::string failure = data.failure;
	// A column without an Analytics type can be named but not read.
	auto &unmapped = data.shape->unmapped;
	for (auto id : input.column_ids) {
		if (duckdb::IsVirtualColumn(id) && id - duckdb::VIRTUAL_COLUMN_START < unmapped.size()) {
			auto &column = unmapped[id - duckdb::VIRTUAL_COLUMN_START];
			throw duckdb::InvalidInputException("ScalarDB Analytics cannot read column %s of %s: %s", column.first,
			                                    data.shape->binding.back(), column.second);
		}
	}
	if (!adopted && !data.offered) {
		// The optimizer never offered the region; read the columns DuckDB
		// asks for.
		adopted = Plan(context, data, ReadColumns(input.column_ids), {}, failure);
	}
	if (!adopted) {
		throw duckdb::IOException("ScalarDB Analytics cannot read %s: %s",
		                          data.shape->binding.back(), failure);
	}
	auto state = duckdb::make_uniq<ScanGlobalState>(data.data_source, data.shape, adopted);
	for (auto id : input.column_ids) {
		if (id == duckdb::COLUMN_IDENTIFIER_EMPTY) {
			state->outputs.push_back({0, true});
			continue;
		}
		if (duckdb::IsVirtualColumn(id)) {
			// The scan declares no other virtual column (VirtualColumns).
			throw duckdb::InternalException("ScalarDB Analytics scan was asked for virtual column %llu", id);
		}
		auto position = std::find(adopted->columns.begin(), adopted->columns.end(), id);
		if (position == adopted->columns.end()) {
			throw duckdb::InternalException("ScalarDB Analytics scan was asked for a column it did not plan");
		}
		state->outputs.push_back({static_cast<duckdb::idx_t>(position - adopted->columns.begin()), false});
	}
	auto &api = data.data_source->api;
	try {
		state->lease.Check(
		    api.read_start(state->lease.session, adopted->remote_execution, Milliseconds(context, READ_TIMEOUT),
		                   &state->operation, &state->stream),
		    "start the read");
	} catch (ConnectorException &error) {
		auto stale = Stale(*data.shape, error.kind);
		if (stale.empty()) {
			throw;
		}
		throw ConnectorException(error.kind, duckdb::ErrorData(error).RawMessage() + stale);
	}
	ReadSchema(context, *state);
	return std::move(state);
}

struct ScanLocalState : public duckdb::LocalTableFunctionState {
	ScanLocalState(duckdb::ClientContext &context, const ScanGlobalState &global)
	    : projection(context, global.adopted->program.projection) {
		auto &program = global.adopted->program;
		if (program.filter) {
			filter = duckdb::make_uniq<duckdb::ExpressionExecutor>(context, *program.filter);
		}
		auto &arrow_types = const_cast<duckdb::ArrowTableSchema &>(global.arrow_schema).GetTypes();
		storage.Initialize(duckdb::Allocator::DefaultAllocator(), arrow_types);
		duckdb::vector<duckdb::LogicalType> remote_types;
		for (auto &field : global.adopted->remote_output) {
			remote_types.push_back(DuckDBType(field));
		}
		remote.Initialize(duckdb::Allocator::DefaultAllocator(), remote_types);
		duckdb::vector<duckdb::LogicalType> output_types;
		for (auto &expression : program.projection) {
			output_types.push_back(expression->return_type);
		}
		output.Initialize(duckdb::Allocator::DefaultAllocator(), output_types);
	}

	duckdb::unique_ptr<duckdb::ArrowScanLocalState> batch;
	duckdb::unique_ptr<duckdb::ExpressionExecutor> filter;
	duckdb::ExpressionExecutor projection;
	// The current slice as DuckDB reads Arrow, as Analytics-mapped types, and
	// after the local program.
	duckdb::DataChunk storage;
	duckdb::DataChunk remote;
	duckdb::DataChunk output;
};

duckdb::unique_ptr<duckdb::LocalTableFunctionState> InitLocal(duckdb::ExecutionContext &context,
                                                              duckdb::TableFunctionInitInput &input,
                                                              duckdb::GlobalTableFunctionState *global) {
	return duckdb::make_uniq<ScanLocalState>(context.client, global->Cast<ScanGlobalState>());
}

// Pulls the next Arrow batch; returns false at the end of the stream.
bool NextBatch(duckdb::ClientContext &context, ScanGlobalState &global, ScanLocalState &local) {
	auto wrapper = duckdb::make_uniq<duckdb::ArrowArrayWrapper>();
	if (global.stream.get_next(&global.stream, &wrapper->arrow_array) != 0) {
		throw duckdb::IOException("ScalarDB Analytics read failed: %s", global.Failure());
	}
	if (!wrapper->arrow_array.release) {
		return false;
	}
	local.batch = duckdb::make_uniq<duckdb::ArrowScanLocalState>(std::move(wrapper), context);
	return true;
}

void Scan(duckdb::ClientContext &context, duckdb::TableFunctionInput &input, duckdb::DataChunk &output) {
	auto &global = input.global_state->Cast<ScanGlobalState>();
	auto &local = input.local_state->Cast<ScanLocalState>();
	while (!global.done) {
		if (!local.batch || local.batch->chunk_offset >= static_cast<duckdb::idx_t>(local.batch->chunk->arrow_array.length)) {
			if (!NextBatch(context, global, local)) {
				global.done = true;
				break;
			}
			continue;
		}
		auto &batch = *local.batch;
		auto rows = duckdb::MinValue<duckdb::idx_t>(STANDARD_VECTOR_SIZE,
		                                           batch.chunk->arrow_array.length - batch.chunk_offset);
		local.storage.Reset();
		local.storage.SetCardinality(rows);
		duckdb::ArrowTableFunction::ArrowToDuckDB(batch, global.arrow_schema.GetColumns(), local.storage);
		batch.chunk_offset += rows;

		// Storage types to the mapped types: Uuid and Enum arrive as text, and
		// negative-scale decimals as unscaled integers.
		local.remote.Reset();
		for (duckdb::idx_t i = 0; i < local.remote.ColumnCount(); i++) {
			auto &source = local.storage.data[i];
			auto &target = local.remote.data[i];
			if (source.GetType() == target.GetType()) {
				target.Reference(source);
			} else {
				duckdb::VectorOperations::DefaultCast(source, target, rows);
			}
		}
		local.remote.SetCardinality(rows);
		for (auto &scaled : global.scaled) {
			auto &vector = local.remote.data[scaled.first];
			vector.Flatten(rows);
			auto values = duckdb::FlatVector::GetData<double>(vector);
			for (duckdb::idx_t row = 0; row < rows; row++) {
				values[row] *= scaled.second;
			}
		}

		if (local.filter) {
			duckdb::SelectionVector selection(STANDARD_VECTOR_SIZE);
			auto count = local.filter->SelectExpression(local.remote, selection);
			if (count == 0) {
				continue;
			}
			if (count < rows) {
				local.remote.Slice(selection, count);
			}
		}
		local.output.Reset();
		local.projection.Execute(local.remote, local.output);
		for (duckdb::idx_t i = 0; i < global.outputs.size(); i++) {
			auto &column = global.outputs[i];
			if (column.empty) {
				output.data[i].Reference(duckdb::Value(output.data[i].GetType()));
			} else {
				output.data[i].Reference(local.output.data[column.position]);
			}
		}
		output.SetCardinality(local.output.size());
		return;
	}
	output.SetCardinality(0);
}

// The scan's virtual columns are the table's (VirtualColumns in the catalog):
// DuckDB names columns by the table entry's and reads them by the scan's.
duckdb::virtual_column_map_t ScanVirtualColumns(duckdb::ClientContext &,
                                                duckdb::optional_ptr<duckdb::FunctionData> bind_data) {
	return VirtualColumns(*bind_data->Cast<ScanData>().shape);
}

duckdb::InsertionOrderPreservingMap<std::string> ToString(duckdb::TableFunctionToStringInput &input) {
	duckdb::InsertionOrderPreservingMap<std::string> result;
	auto &data = input.bind_data->Cast<ScanData>();
	std::string table;
	for (auto &name : data.shape->binding) {
		table += (table.empty() ? "" : ".") + name;
	}
	result["Table"] = table;
	if (data.adopted) {
		result["Local Plan"] = data.adopted->local_plan ? "yes" : "no";
		result["Local Filter"] = data.adopted->program.filter ? "yes" : "no";
	}
	return result;
}

// The catalog table the scan reads. DuckDB then names the scan's columns by
// the table entry, its virtual columns included, and treats the scan as a
// read of that table, which is not a DuckDB table.
duckdb::BindInfo BindInfo(const duckdb::optional_ptr<duckdb::FunctionData> bind_data) {
	return duckdb::BindInfo(bind_data->Cast<ScanData>().table);
}

} // namespace

duckdb::TableFunction ScanFunction() {
	duckdb::TableFunction function("scalardb_analytics_scan", {}, Scan);
	function.init_global = InitGlobal;
	function.init_local = InitLocal;
	function.pushdown_complex_filter = PushdownComplexFilter;
	function.to_string = ToString;
	function.get_virtual_columns = ScanVirtualColumns;
	function.get_bind_info = BindInfo;
	function.projection_pushdown = true;
	return function;
}

duckdb::unique_ptr<duckdb::FunctionData> ScanBindData(std::shared_ptr<DataSource> data_source,
                                                      std::shared_ptr<const TableShape> shape,
                                                      duckdb::TableCatalogEntry &table) {
	return duckdb::make_uniq<ScanData>(std::move(data_source), std::move(shape), table);
}

} // namespace scalardb_analytics_duckdb
