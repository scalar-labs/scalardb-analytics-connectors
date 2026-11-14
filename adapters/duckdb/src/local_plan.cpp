#include "local_plan.hpp"

#include <map>

#include "duckdb/common/types/decimal.hpp"
#include "duckdb/planner/expression/list.hpp"
#include "offered_plan.hpp"

namespace scalardb_analytics_duckdb {

namespace {

const char *const REMOTE_RESULT_TYPE_URL = "urn:scalardb:analytics:substrait:remote-result-schema:v1";

// A planned result that breaks the contract, which only a library defect
// produces. Its plans reference only columns they have, and the local plan's
// operators are the offered ones the result left local, over a RemoteResult
// that is the remote output, producing the requested columns.
[[noreturn]] void Violation(const std::string &what) {
	throw duckdb::IOException("ScalarDB Analytics returned a planned result that breaks the contract: %s", what);
}

struct Declared {
	std::string uri;
	std::string name;
};

// The function declarations of a plan by anchor.
std::map<uint32_t, Declared> Functions(const substrait::Plan &plan) {
	std::map<uint32_t, std::string> uris;
	for (auto &uri : plan.extension_uris()) {
		uris[uri.extension_uri_anchor()] = uri.uri();
	}
	std::map<uint32_t, Declared> functions;
	for (auto &extension : plan.extensions()) {
		if (extension.has_extension_function()) {
			auto &function = extension.extension_function();
			functions[function.function_anchor()] = {uris[function.extension_uri_reference()], function.name()};
		}
	}
	return functions;
}

std::vector<std::string> RootNames(const std::vector<Field> &fields) {
	std::vector<std::string> names;
	for (auto &field : fields) {
		names.push_back(field.name);
		names.insert(names.end(), field.nested_names.begin(), field.nested_names.end());
	}
	return names;
}

bool DirectField(const substrait::Expression &expression, duckdb::idx_t &position) {
	if (!expression.has_selection() || !expression.selection().has_direct_reference() ||
	    !expression.selection().direct_reference().has_struct_field() ||
	    expression.selection().direct_reference().struct_field().has_child()) {
		return false;
	}
	position = expression.selection().direct_reference().struct_field().field();
	return true;
}

// Lowers expressions of a local plan over the remote output's columns.
class Lowering {
public:
	Lowering(duckdb::ClientContext &context_p, const substrait::Plan &plan, const std::vector<Field> &remote_p)
	    : context(context_p), functions(Functions(plan)), remote(remote_p) {
	}

	duckdb::unique_ptr<duckdb::Expression> Lower(const substrait::Expression &expression) {
		duckdb::idx_t position;
		if (DirectField(expression, position)) {
			if (position >= remote.size()) {
				Violation("the local plan references a column outside the remote output");
			}
			return duckdb::make_uniq<duckdb::BoundReferenceExpression>(DuckDBType(remote[position]), position);
		}
		if (expression.has_literal()) {
			return duckdb::make_uniq<duckdb::BoundConstantExpression>(Constant(expression.literal()));
		}
		if (expression.has_scalar_function()) {
			return Call(expression.scalar_function());
		}
		throw Unlowerable("an expression outside the local plan profile");
	}

	struct Unlowerable : std::runtime_error {
		using std::runtime_error::runtime_error;
	};

private:
	duckdb::Value Constant(const substrait::Expression_Literal &literal) {
		using duckdb::Value;
		switch (literal.literal_type_case()) {
		case substrait::Expression_Literal::kBoolean:
			return Value::BOOLEAN(literal.boolean());
		case substrait::Expression_Literal::kI8:
			return Value::TINYINT(static_cast<int8_t>(literal.i8()));
		case substrait::Expression_Literal::kI16:
			return Value::SMALLINT(static_cast<int16_t>(literal.i16()));
		case substrait::Expression_Literal::kI32:
			return Value::INTEGER(literal.i32());
		case substrait::Expression_Literal::kI64:
			return Value::BIGINT(literal.i64());
		case substrait::Expression_Literal::kFp32:
			return Value::FLOAT(literal.fp32());
		case substrait::Expression_Literal::kFp64:
			return Value::DOUBLE(literal.fp64());
		case substrait::Expression_Literal::kString:
			return Value(literal.string());
		case substrait::Expression_Literal::kBinary:
			return Value::BLOB(reinterpret_cast<duckdb::const_data_ptr_t>(literal.binary().data()),
			                   literal.binary().size());
		case substrait::Expression_Literal::kDate:
			return Value::DATE(duckdb::date_t(literal.date()));
		case substrait::Expression_Literal::kPrecisionTime:
			return Value::TIME(duckdb::dtime_t(literal.precision_time().value()));
		case substrait::Expression_Literal::kPrecisionTimestamp:
			return Value::TIMESTAMP(duckdb::timestamp_t(literal.precision_timestamp().value()));
		case substrait::Expression_Literal::kPrecisionTimestampTz:
			return Value::TIMESTAMPTZ(duckdb::timestamp_tz_t(literal.precision_timestamp_tz().value()));
		case substrait::Expression_Literal::kDecimal: {
			auto &decimal = literal.decimal();
			if (decimal.value().size() != 16 || decimal.scale() < 0) {
				break;
			}
			auto bytes = reinterpret_cast<const uint8_t *>(decimal.value().data());
			uint64_t lower = 0, upper = 0;
			for (int i = 7; i >= 0; i--) {
				lower = (lower << 8) | bytes[i];
				upper = (upper << 8) | bytes[8 + i];
			}
			auto width = static_cast<uint8_t>(decimal.precision());
			auto scale = static_cast<uint8_t>(decimal.scale());
			// DuckDB stores a decimal of width up to 18 in 64 bits, where the
			// unscaled value always fits.
			if (width <= duckdb::Decimal::MAX_WIDTH_INT64) {
				return Value::DECIMAL(static_cast<int64_t>(lower), width, scale);
			}
			return Value::DECIMAL(duckdb::hugeint_t(static_cast<int64_t>(upper), lower), width, scale);
		}
		case substrait::Expression_Literal::kUserDefined: {
			auto &logical = literal.user_defined();
			if (!logical.has_struct_() || logical.struct_().fields_size() != 1) {
				break;
			}
			Field field;
			auto &type = *field.type.mutable_user_defined();
			type.set_type_reference(logical.type_reference());
			*type.mutable_type_parameters() = logical.type_parameters();
			if (LogicalName(field.type).empty()) {
				break;
			}
			return Constant(logical.struct_().fields(0)).DefaultCastAs(DuckDBType(field));
		}
		default:
			break;
		}
		throw Unlowerable("a literal outside the local plan profile");
	}

	duckdb::unique_ptr<duckdb::Expression> Call(const substrait::Expression_ScalarFunction &call) {
		auto declared = functions.find(call.function_reference());
		if (declared == functions.end()) {
			throw Unlowerable("an undeclared function");
		}
		duckdb::vector<duckdb::unique_ptr<duckdb::Expression>> arguments;
		for (auto &argument : call.arguments()) {
			if (!argument.has_value()) {
				throw Unlowerable("a function argument that is not a value");
			}
			arguments.push_back(Lower(argument.value()));
		}
		auto &uri = declared->second.uri;
		auto &name = declared->second.name;
		auto arity = [&](size_t count) {
			if (arguments.size() != count) {
				throw Unlowerable("a function with an unexpected argument count");
			}
		};
		if (uri == COMPARISON_FUNCTIONS_URI) {
			static const std::map<std::string, duckdb::ExpressionType> comparisons = {
			    {"equal:any1_any1", duckdb::ExpressionType::COMPARE_EQUAL},
			    {"not_equal:any1_any1", duckdb::ExpressionType::COMPARE_NOTEQUAL},
			    {"lt:any1_any1", duckdb::ExpressionType::COMPARE_LESSTHAN},
			    {"lte:any1_any1", duckdb::ExpressionType::COMPARE_LESSTHANOREQUALTO},
			    {"gt:any1_any1", duckdb::ExpressionType::COMPARE_GREATERTHAN},
			    {"gte:any1_any1", duckdb::ExpressionType::COMPARE_GREATERTHANOREQUALTO},
			};
			auto comparison = comparisons.find(name);
			if (comparison != comparisons.end()) {
				arity(2);
				if (arguments[0]->return_type != arguments[1]->return_type) {
					throw Unlowerable("a comparison of different types");
				}
				return duckdb::make_uniq<duckdb::BoundComparisonExpression>(
				    comparison->second, std::move(arguments[0]), std::move(arguments[1]));
			}
			if (name == "is_null:any" || name == "is_not_null:any") {
				arity(1);
				auto type = name == "is_null:any" ? duckdb::ExpressionType::OPERATOR_IS_NULL
				                                  : duckdb::ExpressionType::OPERATOR_IS_NOT_NULL;
				auto operation = duckdb::make_uniq<duckdb::BoundOperatorExpression>(type, duckdb::LogicalType::BOOLEAN);
				operation->children.push_back(std::move(arguments[0]));
				return std::move(operation);
			}
		} else if (uri == BOOLEAN_FUNCTIONS_URI) {
			for (auto &argument : arguments) {
				if (argument->return_type != duckdb::LogicalType::BOOLEAN) {
					throw Unlowerable("a Boolean function over another type");
				}
			}
			if (name == "and:bool_bool" || name == "or:bool_bool") {
				arity(2);
				auto type = name == "and:bool_bool" ? duckdb::ExpressionType::CONJUNCTION_AND
				                                    : duckdb::ExpressionType::CONJUNCTION_OR;
				return duckdb::make_uniq<duckdb::BoundConjunctionExpression>(type, std::move(arguments[0]),
				                                                             std::move(arguments[1]));
			}
			if (name == "not:bool") {
				arity(1);
				auto operation = duckdb::make_uniq<duckdb::BoundOperatorExpression>(
				    duckdb::ExpressionType::OPERATOR_NOT, duckdb::LogicalType::BOOLEAN);
				operation->children.push_back(std::move(arguments[0]));
				return std::move(operation);
			}
		}
		throw Unlowerable("function " + name + " of " + uri);
	}

	duckdb::ClientContext &context;
	std::map<uint32_t, Declared> functions;
	const std::vector<Field> &remote;
};

bool Reject(std::string &reason, const std::string &why) {
	reason = why;
	return false;
}

} // namespace

bool RemoteOutput(const substrait::Plan &remote_plan, std::vector<Field> &output) {
	if (remote_plan.relations_size() != 1 || !remote_plan.relations(0).has_root()) {
		return false;
	}
	auto &root = remote_plan.relations(0).root();
	const substrait::Rel *relation = &root.input();
	const substrait::ProjectRel *project = nullptr;
	if (relation->has_project()) {
		project = &relation->project();
		relation = &project->input();
	}
	if (relation->has_filter()) {
		relation = &relation->filter().input();
	}
	if (!relation->has_read()) {
		return false;
	}
	auto fields = FieldsOf(relation->read().base_schema());
	if (project) {
		auto inputs = fields;
		for (auto &expression : project->expressions()) {
			duckdb::idx_t position;
			if (!DirectField(expression, position)) {
				return false;
			}
			if (position >= fields.size()) {
				Violation("the remote projection references a column outside its read");
			}
			inputs.push_back(fields[position]);
		}
		fields.clear();
		for (auto position : project->common().emit().output_mapping()) {
			if (position < 0 || static_cast<size_t>(position) >= inputs.size()) {
				Violation("the remote projection selects a column outside its input");
			}
			fields.push_back(inputs[position]);
		}
	}
	// The root names rename the output.
	substrait::NamedStruct renamed = NamedStructOf(fields);
	if (renamed.names_size() != root.names_size()) {
		Violation("the remote plan's root names do not match its output");
	}
	*renamed.mutable_names() = root.names();
	output = FieldsOf(renamed);
	return true;
}

void IdentityProgram(const std::vector<Field> &remote, const std::vector<Field> &requested, LocalProgram &program) {
	if (remote.size() != requested.size() || RootNames(remote) != RootNames(requested)) {
		Violation("the remote output without a local plan is not the requested columns");
	}
	for (duckdb::idx_t i = 0; i < remote.size(); i++) {
		if (!SameType(remote[i].type, requested[i].type)) {
			Violation("the remote output column " + remote[i].name + " has another type than requested");
		}
		program.projection.push_back(duckdb::make_uniq<duckdb::BoundReferenceExpression>(DuckDBType(remote[i]), i));
	}
}

bool LowerLocalPlan(duckdb::ClientContext &context, const substrait::Plan &local_plan, const std::vector<Field> &remote,
                    const std::vector<Field> &requested, LocalProgram &program, std::string &reason) {
	if (local_plan.relations_size() != 1 || !local_plan.relations(0).has_root()) {
		Violation("the local plan has no single root");
	}
	auto &root = local_plan.relations(0).root();
	const substrait::Rel *relation = &root.input();
	const substrait::ProjectRel *project = nullptr;
	const substrait::FilterRel *filter = nullptr;
	if (relation->has_project()) {
		project = &relation->project();
		relation = &project->input();
	}
	if (relation->has_filter()) {
		filter = &relation->filter();
		relation = &filter->input();
	}
	if (!relation->has_extension_leaf() || relation->extension_leaf().detail().type_url() != REMOTE_RESULT_TYPE_URL) {
		Violation("the local plan's operators are not over the remote result");
	}
	substrait::NamedStruct leaf_schema;
	if (!leaf_schema.ParseFromString(relation->extension_leaf().detail().value())) {
		Violation("the remote result schema cannot be parsed");
	}
	auto leaf = FieldsOf(leaf_schema);
	if (leaf.size() != remote.size() || RootNames(leaf) != RootNames(remote)) {
		Violation("the remote result differs from the remote plan's output");
	}
	for (duckdb::idx_t i = 0; i < leaf.size(); i++) {
		if (!SameType(leaf[i].type, remote[i].type)) {
			Violation("the remote result differs from the remote plan's output");
		}
	}
	if (std::vector<std::string>(root.names().begin(), root.names().end()) != RootNames(requested)) {
		Violation("the local plan's output names are not the requested columns");
	}
	Lowering lowering(context, local_plan, remote);
	try {
		if (filter) {
			program.filter = lowering.Lower(filter->condition());
			if (program.filter->return_type != duckdb::LogicalType::BOOLEAN) {
				Violation("the local filter is not Boolean");
			}
		}
		duckdb::vector<duckdb::unique_ptr<duckdb::Expression>> inputs;
		for (duckdb::idx_t i = 0; i < remote.size(); i++) {
			inputs.push_back(duckdb::make_uniq<duckdb::BoundReferenceExpression>(DuckDBType(remote[i]), i));
		}
		if (project) {
			for (auto &expression : project->expressions()) {
				inputs.push_back(lowering.Lower(expression));
			}
			for (auto position : project->common().emit().output_mapping()) {
				if (position < 0 || static_cast<size_t>(position) >= inputs.size()) {
					Violation("the local projection selects a column outside its input");
				}
				if (!inputs[position]) {
					// Valid Substrait, but each lowered expression is moved
					// into one output column.
					return Reject(reason, "the local projection selects a column twice");
				}
				program.projection.push_back(std::move(inputs[position]));
			}
		} else {
			program.projection = std::move(inputs);
		}
	} catch (Lowering::Unlowerable &error) {
		// Only the adapter's lowering limits reject a proposal. Every other
		// failure means a Connector or library broke the contract and stays a
		// query error: the remote output's Analytics types come from the
		// table's base schema, which mapped when the table was bound, and
		// the local plan's literals are values of their types.
		return Reject(reason, std::string("the local plan contains ") + error.what());
	}
	if (program.projection.size() != requested.size()) {
		Violation("the local plan's output is not the requested columns");
	}
	for (duckdb::idx_t i = 0; i < requested.size(); i++) {
		if (program.projection[i]->return_type != DuckDBType(requested[i])) {
			Violation("the local plan's output column " + requested[i].name + " has another type than requested");
		}
	}
	return true;
}

} // namespace scalardb_analytics_duckdb
