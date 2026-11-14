#include "offered_plan.hpp"

#include "duckdb/planner/expression/list.hpp"

namespace scalardb_analytics_duckdb {

const char *const COMPARISON_FUNCTIONS_URI =
    "https://github.com/substrait-io/substrait/blob/v0.69.0/extensions/functions_comparison.yaml";
const char *const BOOLEAN_FUNCTIONS_URI =
    "https://github.com/substrait-io/substrait/blob/v0.69.0/extensions/functions_boolean.yaml";

namespace {

// The function anchors every offered plan declares.
enum Function : uint32_t {
	EQUAL = 1,
	NOT_EQUAL,
	LESS,
	LESS_OR_EQUAL,
	GREATER,
	GREATER_OR_EQUAL,
	IS_NULL,
	IS_NOT_NULL,
	AND,
	OR,
	NOT,
};

struct Declaration {
	Function anchor;
	uint32_t uri_anchor;
	const char *name;
};

const Declaration DECLARATIONS[] = {
    {EQUAL, 1, "equal:any1_any1"},  {NOT_EQUAL, 1, "not_equal:any1_any1"},
    {LESS, 1, "lt:any1_any1"},      {LESS_OR_EQUAL, 1, "lte:any1_any1"},
    {GREATER, 1, "gt:any1_any1"},   {GREATER_OR_EQUAL, 1, "gte:any1_any1"},
    {IS_NULL, 1, "is_null:any"},    {IS_NOT_NULL, 1, "is_not_null:any"},
    {AND, 2, "and:bool_bool"},      {OR, 2, "or:bool_bool"},
    {NOT, 2, "not:bool"},
};

substrait::Expression FieldReference(duckdb::idx_t position) {
	substrait::Expression expression;
	auto &selection = *expression.mutable_selection();
	selection.mutable_direct_reference()->mutable_struct_field()->set_field(static_cast<int32_t>(position));
	selection.mutable_root_reference();
	return expression;
}

substrait::Expression Call(Function function, std::vector<substrait::Expression> arguments) {
	substrait::Expression expression;
	auto &call = *expression.mutable_scalar_function();
	call.set_function_reference(function);
	for (auto &argument : arguments) {
		*call.add_arguments()->mutable_value() = std::move(argument);
	}
	call.mutable_output_type()->mutable_bool_()->set_nullability(substrait::Type::NULLABILITY_NULLABLE);
	return expression;
}

// The base column a DuckDB column reference of this scan reads, or none.
bool BaseColumn(const duckdb::Expression &expression, const duckdb::LogicalGet &get, duckdb::idx_t &column) {
	if (expression.GetExpressionClass() != duckdb::ExpressionClass::BOUND_COLUMN_REF) {
		return false;
	}
	auto &reference = expression.Cast<duckdb::BoundColumnRefExpression>();
	auto &ids = get.GetColumnIds();
	if (reference.binding.table_index != get.table_index || reference.binding.column_index >= ids.size()) {
		return false;
	}
	auto &id = ids[reference.binding.column_index];
	if (!id.HasPrimaryIndex() || id.IsVirtualColumn() || id.HasChildren()) {
		return false;
	}
	column = id.GetPrimaryIndex();
	return true;
}

// Whether the Analytics type's order, not only its equality, is defined and
// shared by DuckDB's comparison of the mapped type.
bool Ordered(const substrait::Type &type) {
	switch (type.kind_case()) {
	case substrait::Type::kBool:
	case substrait::Type::kI8:
	case substrait::Type::kI16:
	case substrait::Type::kI32:
	case substrait::Type::kI64:
	case substrait::Type::kFp32:
	case substrait::Type::kFp64:
	case substrait::Type::kString:
	case substrait::Type::kBinary:
	case substrait::Type::kDate:
	case substrait::Type::kPrecisionTime:
	case substrait::Type::kPrecisionTimestamp:
	case substrait::Type::kPrecisionTimestampTz:
		return true;
	case substrait::Type::kDecimal:
		return type.decimal().scale() >= 0;
	default:
		return false;
	}
}

// Whether equality of the type can be offered: the ordered types and the
// extension-encoded types, whose equality is that of their carrier.
bool Comparable(const substrait::Type &type) {
	return Ordered(type) || type.kind_case() == substrait::Type::kUserDefined;
}

void LittleEndian(duckdb::hugeint_t value, std::string &bytes) {
	bytes.resize(16);
	auto lower = value.lower;
	auto upper = static_cast<uint64_t>(value.upper);
	for (int i = 0; i < 8; i++) {
		bytes[i] = static_cast<char>((lower >> (8 * i)) & 0xff);
		bytes[8 + i] = static_cast<char>((upper >> (8 * i)) & 0xff);
	}
}

// Encodes a constant as a literal of the column's Analytics type.
bool Literal(const duckdb::Value &value, const Field &field, substrait::Expression_Literal &literal) {
	if (value.IsNull() || value.type() != DuckDBType(field)) {
		return false;
	}
	auto &type = field.type;
	switch (type.kind_case()) {
	case substrait::Type::kBool:
		literal.set_boolean(value.GetValue<bool>());
		return true;
	case substrait::Type::kI8:
		literal.set_i8(value.GetValue<int8_t>());
		return true;
	case substrait::Type::kI16:
		literal.set_i16(value.GetValue<int16_t>());
		return true;
	case substrait::Type::kI32:
		literal.set_i32(value.GetValue<int32_t>());
		return true;
	case substrait::Type::kI64:
		literal.set_i64(value.GetValue<int64_t>());
		return true;
	case substrait::Type::kFp32:
		literal.set_fp32(value.GetValue<float>());
		return true;
	case substrait::Type::kFp64:
		literal.set_fp64(value.GetValue<double>());
		return true;
	case substrait::Type::kDecimal: {
		duckdb::hugeint_t unscaled;
		switch (value.type().InternalType()) {
		case duckdb::PhysicalType::INT16:
			unscaled = value.GetValueUnsafe<int16_t>();
			break;
		case duckdb::PhysicalType::INT32:
			unscaled = value.GetValueUnsafe<int32_t>();
			break;
		case duckdb::PhysicalType::INT64:
			unscaled = value.GetValueUnsafe<int64_t>();
			break;
		default:
			unscaled = value.GetValueUnsafe<duckdb::hugeint_t>();
		}
		auto &decimal = *literal.mutable_decimal();
		LittleEndian(unscaled, *decimal.mutable_value());
		decimal.set_precision(type.decimal().precision());
		decimal.set_scale(type.decimal().scale());
		return true;
	}
	case substrait::Type::kString:
	case substrait::Type::kBinary: {
		auto &bytes = duckdb::StringValue::Get(value);
		type.kind_case() == substrait::Type::kString ? literal.set_string(bytes) : literal.set_binary(bytes);
		return true;
	}
	case substrait::Type::kDate:
		literal.set_date(value.GetValue<duckdb::date_t>().days);
		return true;
	case substrait::Type::kPrecisionTime: {
		// DuckDB accepts 24:00:00, which a Substrait time, confined to one
		// day, cannot hold.
		auto micros = value.GetValue<duckdb::dtime_t>().micros;
		if (micros >= duckdb::Interval::MICROS_PER_DAY) {
			return false;
		}
		literal.mutable_precision_time()->set_precision(6);
		literal.mutable_precision_time()->set_value(micros);
		return true;
	}
	case substrait::Type::kPrecisionTimestamp:
		literal.mutable_precision_timestamp()->set_precision(6);
		literal.mutable_precision_timestamp()->set_value(value.GetValue<duckdb::timestamp_t>().value);
		return true;
	case substrait::Type::kPrecisionTimestampTz:
		literal.mutable_precision_timestamp_tz()->set_precision(6);
		literal.mutable_precision_timestamp_tz()->set_value(value.GetValue<duckdb::timestamp_tz_t>().value);
		return true;
	case substrait::Type::kUserDefined: {
		auto &logical = *literal.mutable_user_defined();
		logical.set_type_reference(type.user_defined().type_reference());
		*logical.mutable_type_parameters() = type.user_defined().type_parameters();
		auto &carrier = *logical.mutable_struct_()->add_fields();
		if (LogicalName(type) == "year") {
			carrier.set_i16(value.GetValue<int16_t>());
		} else {
			// Uuid in its canonical lowercase form, Enum as its label, and
			// Json and TimeWithTimeZone as the text DuckDB holds.
			carrier.set_string(value.ToString());
		}
		return true;
	}
	default:
		return false;
	}
}

// The Enum column an expression reads as its label: DuckDB binds an Enum
// column compared with a string as `CAST(e AS VARCHAR)`, whose equality is
// that of the labels, which is Enum equality.
bool EnumLabel(const duckdb::Expression &expression, const duckdb::LogicalGet &get, const TableShape &shape,
               duckdb::idx_t &column) {
	if (expression.GetExpressionClass() != duckdb::ExpressionClass::BOUND_CAST) {
		return false;
	}
	auto &cast = expression.Cast<duckdb::BoundCastExpression>();
	return !cast.try_cast && cast.return_type.id() == duckdb::LogicalTypeId::VARCHAR &&
	       BaseColumn(*cast.child, get, column) && LogicalName(shape.fields[column].type) == "enum";
}

// An operand of a comparison: a base column, an Enum column read as its
// label, or a constant typed by the column on the other side. A string
// compared with an Enum label is that Enum's member, or the comparison is
// not offered.
bool Operand(const duckdb::Expression &expression, const duckdb::LogicalGet &get, const TableShape &shape,
             const Field *peer, substrait::Expression &out, const Field *&field) {
	duckdb::idx_t column;
	if (BaseColumn(expression, get, column) || EnumLabel(expression, get, shape, column)) {
		field = &shape.fields[column];
		out = FieldReference(column);
		return true;
	}
	if (expression.GetExpressionClass() == duckdb::ExpressionClass::BOUND_CONSTANT && peer) {
		field = peer;
		auto value = expression.Cast<duckdb::BoundConstantExpression>().value;
		if (LogicalName(peer->type) == "enum" && value.type().id() == duckdb::LogicalTypeId::VARCHAR &&
		    !value.DefaultTryCastAs(DuckDBType(*peer), true)) {
			return false;
		}
		return Literal(value, *peer, *out.mutable_literal());
	}
	return false;
}

bool Comparison(duckdb::ExpressionType type, const duckdb::Expression &left, const duckdb::Expression &right,
                const duckdb::LogicalGet &get, const TableShape &shape, substrait::Expression &out) {
	Function function;
	switch (type) {
	case duckdb::ExpressionType::COMPARE_EQUAL:
		function = EQUAL;
		break;
	case duckdb::ExpressionType::COMPARE_NOTEQUAL:
		function = NOT_EQUAL;
		break;
	case duckdb::ExpressionType::COMPARE_LESSTHAN:
		function = LESS;
		break;
	case duckdb::ExpressionType::COMPARE_LESSTHANOREQUALTO:
		function = LESS_OR_EQUAL;
		break;
	case duckdb::ExpressionType::COMPARE_GREATERTHAN:
		function = GREATER;
		break;
	case duckdb::ExpressionType::COMPARE_GREATERTHANOREQUALTO:
		function = GREATER_OR_EQUAL;
		break;
	default:
		return false;
	}
	// A column operand types a constant on the other side.
	const Field *left_field = nullptr;
	const Field *right_field = nullptr;
	substrait::Expression left_out, right_out;
	duckdb::idx_t column;
	bool left_is_column = BaseColumn(left, get, column) || EnumLabel(left, get, shape, column);
	if (left_is_column) {
		if (!Operand(left, get, shape, nullptr, left_out, left_field) ||
		    !Operand(right, get, shape, left_field, right_out, right_field)) {
			return false;
		}
	} else if (!Operand(right, get, shape, nullptr, right_out, right_field) ||
	           !Operand(left, get, shape, right_field, left_out, left_field)) {
		return false;
	}
	if (!SameType(left_field->type, right_field->type)) {
		return false;
	}
	bool equality = function == EQUAL || function == NOT_EQUAL;
	if (!(equality ? Comparable(left_field->type) : Ordered(left_field->type))) {
		return false;
	}
	out = Call(function, {std::move(left_out), std::move(right_out)});
	return true;
}

bool Translate(const duckdb::Expression &expression, const duckdb::LogicalGet &get, const TableShape &shape,
               substrait::Expression &out) {
	switch (expression.GetExpressionClass()) {
	case duckdb::ExpressionClass::BOUND_COMPARISON: {
		auto &comparison = expression.Cast<duckdb::BoundComparisonExpression>();
		return Comparison(comparison.GetExpressionType(), *comparison.left, *comparison.right, get, shape, out);
	}
	case duckdb::ExpressionClass::BOUND_BETWEEN: {
		auto &between = expression.Cast<duckdb::BoundBetweenExpression>();
		substrait::Expression lower, upper;
		if (!Comparison(between.LowerComparisonType(), *between.input, *between.lower, get, shape, lower) ||
		    !Comparison(between.UpperComparisonType(), *between.input, *between.upper, get, shape, upper)) {
			return false;
		}
		out = Call(AND, {std::move(lower), std::move(upper)});
		return true;
	}
	case duckdb::ExpressionClass::BOUND_CONJUNCTION: {
		auto &conjunction = expression.Cast<duckdb::BoundConjunctionExpression>();
		auto function = conjunction.GetExpressionType() == duckdb::ExpressionType::CONJUNCTION_AND ? AND : OR;
		for (duckdb::idx_t i = 0; i < conjunction.children.size(); i++) {
			substrait::Expression child;
			if (!Translate(*conjunction.children[i], get, shape, child)) {
				return false;
			}
			out = i == 0 ? std::move(child) : Call(function, {std::move(out), std::move(child)});
		}
		return true;
	}
	case duckdb::ExpressionClass::BOUND_OPERATOR: {
		auto &operation = expression.Cast<duckdb::BoundOperatorExpression>();
		switch (operation.GetExpressionType()) {
		case duckdb::ExpressionType::OPERATOR_IS_NULL:
		case duckdb::ExpressionType::OPERATOR_IS_NOT_NULL: {
			duckdb::idx_t column;
			if (!BaseColumn(*operation.children[0], get, column)) {
				return false;
			}
			auto function =
			    operation.GetExpressionType() == duckdb::ExpressionType::OPERATOR_IS_NULL ? IS_NULL : IS_NOT_NULL;
			out = Call(function, {FieldReference(column)});
			return true;
		}
		case duckdb::ExpressionType::OPERATOR_NOT: {
			substrait::Expression child;
			if (!Translate(*operation.children[0], get, shape, child)) {
				return false;
			}
			out = Call(NOT, {std::move(child)});
			return true;
		}
		case duckdb::ExpressionType::COMPARE_IN: {
			// `x IN (a, b)` is `x = a OR x = b`.
			for (duckdb::idx_t i = 1; i < operation.children.size(); i++) {
				substrait::Expression equal;
				if (!Comparison(duckdb::ExpressionType::COMPARE_EQUAL, *operation.children[0],
				                *operation.children[i], get, shape, equal)) {
					return false;
				}
				out = i == 1 ? std::move(equal) : Call(OR, {std::move(out), std::move(equal)});
			}
			return operation.children.size() > 1;
		}
		default:
			return false;
		}
	}
	default:
		return false;
	}
}

} // namespace

bool TranslateFilter(const duckdb::Expression &filter, const duckdb::LogicalGet &get, const TableShape &shape,
                     substrait::Expression &condition) {
	return Translate(filter, get, shape, condition);
}

substrait::Plan OfferedPlan(const TableShape &shape, const std::vector<duckdb::idx_t> &columns,
                            const std::vector<substrait::Expression> &conditions) {
	substrait::Plan plan;
	const char *uris[] = {COMPARISON_FUNCTIONS_URI, BOOLEAN_FUNCTIONS_URI, LOGICAL_TYPE_URN};
	for (uint32_t i = 0; i < 3; i++) {
		auto &uri = *plan.add_extension_uris();
		uri.set_extension_uri_anchor(i + 1);
		uri.set_uri(uris[i]);
	}
	for (auto &declaration : DECLARATIONS) {
		auto &function = *plan.add_extensions()->mutable_extension_function();
		function.set_extension_uri_reference(declaration.uri_anchor);
		function.set_function_anchor(declaration.anchor);
		function.set_name(declaration.name);
	}
	auto &logical = *plan.add_extensions()->mutable_extension_type();
	logical.set_extension_uri_reference(3);
	logical.set_type_anchor(LOGICAL_TYPE_ANCHOR);
	logical.set_name("logical");

	substrait::Rel read;
	auto &read_rel = *read.mutable_read();
	*read_rel.mutable_base_schema() = NamedStructOf(shape.fields);
	for (auto &name : shape.binding) {
		read_rel.mutable_named_table()->add_names(name);
	}

	substrait::Rel input = std::move(read);
	if (!conditions.empty()) {
		substrait::Rel filter;
		auto &filter_rel = *filter.mutable_filter();
		*filter_rel.mutable_input() = std::move(input);
		auto condition = conditions[0];
		for (size_t i = 1; i < conditions.size(); i++) {
			condition = Call(AND, {std::move(condition), conditions[i]});
		}
		*filter_rel.mutable_condition() = std::move(condition);
		input = std::move(filter);
	}

	auto &root = *plan.add_relations()->mutable_root();
	auto &project = *root.mutable_input()->mutable_project();
	*project.mutable_input() = std::move(input);
	auto &emit = *project.mutable_common()->mutable_emit();
	for (duckdb::idx_t i = 0; i < columns.size(); i++) {
		*project.add_expressions() = FieldReference(columns[i]);
		emit.add_output_mapping(static_cast<int32_t>(shape.fields.size() + i));
		auto &field = shape.fields[columns[i]];
		root.add_names(field.name);
		for (auto &nested : field.nested_names) {
			root.add_names(nested);
		}
	}
	return plan;
}

} // namespace scalardb_analytics_duckdb
