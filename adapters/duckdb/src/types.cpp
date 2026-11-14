#include "types.hpp"

#include <cstring>

#include "connector_api.hpp"
#include "duckdb/common/arrow/schema_metadata.hpp"

namespace scalardb_analytics_duckdb {

const char *const LOGICAL_TYPE_URN = "urn:scalardb:analytics:substrait:logical";

namespace {

using NameCursor = std::vector<std::string>::const_iterator;

substrait::Type_Nullability Nullability(bool nullable) {
	return nullable ? substrait::Type::NULLABILITY_NULLABLE : substrait::Type::NULLABILITY_REQUIRED;
}

// Encodes an extension-encoded type as `logical<name, parameters, carrier>`.
void SetLogical(substrait::Type &type, uint32_t anchor, const std::string &name, const std::string &parameters,
                substrait::Type carrier, bool nullable) {
	auto &logical = *type.mutable_user_defined();
	logical.set_type_reference(anchor);
	logical.set_nullability(Nullability(nullable));
	logical.add_type_parameters()->set_string(name);
	logical.add_type_parameters()->set_string(parameters);
	*logical.add_type_parameters()->mutable_data_type() = std::move(carrier);
}

substrait::Type StringType(bool nullable) {
	substrait::Type type;
	type.mutable_string()->set_nullability(Nullability(nullable));
	return type;
}

std::string JsonString(const std::string &value) {
	std::string result = "\"";
	for (auto c : value) {
		switch (c) {
		case '"':
			result += "\\\"";
			break;
		case '\\':
			result += "\\\\";
			break;
		default:
			if (static_cast<unsigned char>(c) < 0x20) {
				char escaped[8];
				snprintf(escaped, sizeof(escaped), "\\u%04x", c);
				result += escaped;
			} else {
				result += c;
			}
		}
	}
	return result + "\"";
}

substrait::Type Encode(const ScalardbAnalyticsConnectorAnalyticsType &source, bool nullable, uint32_t anchor,
                       std::vector<std::string> &names) {
	substrait::Type type;
	auto n = Nullability(nullable);
	switch (source.kind) {
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_BOOLEAN:
		type.mutable_bool_()->set_nullability(n);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_BYTE:
		type.mutable_i8()->set_nullability(n);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_SMALL_INT:
		type.mutable_i16()->set_nullability(n);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_INT:
		type.mutable_i32()->set_nullability(n);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_BIG_INT:
		type.mutable_i64()->set_nullability(n);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_FLOAT:
		type.mutable_fp32()->set_nullability(n);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_DOUBLE:
		type.mutable_fp64()->set_nullability(n);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_DECIMAL: {
		auto &decimal = *type.mutable_decimal();
		decimal.set_precision(source.decimal_precision);
		decimal.set_scale(source.decimal_scale);
		decimal.set_nullability(n);
		break;
	}
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_TEXT:
		type.mutable_string()->set_nullability(n);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_BLOB:
		type.mutable_binary()->set_nullability(n);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_DATE:
		type.mutable_date()->set_nullability(n);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_TIME:
		type.mutable_precision_time()->set_precision(6);
		type.mutable_precision_time()->set_nullability(n);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_TIMESTAMP:
		type.mutable_precision_timestamp()->set_precision(6);
		type.mutable_precision_timestamp()->set_nullability(n);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_TIMESTAMP_TZ:
		type.mutable_precision_timestamp_tz()->set_precision(6);
		type.mutable_precision_timestamp_tz()->set_nullability(n);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_DURATION:
		type.mutable_interval_day()->set_precision(6);
		type.mutable_interval_day()->set_nullability(n);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_INTERVAL:
		type.mutable_interval_compound()->set_precision(9);
		type.mutable_interval_compound()->set_nullability(n);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_ARRAY: {
		auto &list = *type.mutable_list();
		*list.mutable_type() = Encode(*source.element_type, source.element_nullable, anchor, names);
		list.set_nullability(n);
		break;
	}
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_MAP: {
		auto &map = *type.mutable_map();
		*map.mutable_key() = Encode(*source.key_type, false, anchor, names);
		*map.mutable_value() = Encode(*source.value_type, source.value_nullable, anchor, names);
		map.set_nullability(n);
		break;
	}
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_STRUCT: {
		auto &structure = *type.mutable_struct_();
		for (size_t i = 0; i < source.field_count; i++) {
			auto &field = source.fields[i];
			names.push_back(Text(field.name));
			*structure.add_types() = Encode(*field.data_type, field.nullable, anchor, names);
		}
		structure.set_nullability(n);
		break;
	}
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_UUID:
		SetLogical(type, anchor, "uuid", "{}", StringType(false), nullable);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_JSON:
		SetLogical(type, anchor, "json", "{}", StringType(false), nullable);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_TIME_WITH_TIME_ZONE:
		SetLogical(type, anchor, "timetz", "{}", StringType(false), nullable);
		break;
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_YEAR: {
		substrait::Type carrier;
		carrier.mutable_i16()->set_nullability(substrait::Type::NULLABILITY_REQUIRED);
		SetLogical(type, anchor, "year", "{}", std::move(carrier), nullable);
		break;
	}
	case ScalardbAnalyticsConnectorAnalyticsTypeKind_ENUM: {
		std::string members = "{\"members\":[";
		for (size_t i = 0; i < source.member_count; i++) {
			members += (i ? "," : "") + JsonString(Text(source.members[i]));
		}
		SetLogical(type, anchor, "enum", members + "]}", StringType(false), nullable);
		break;
	}
	default:
		throw duckdb::NotImplementedException("Analytics type kind %d is unknown to the DuckDB adapter",
		                                      static_cast<int>(source.kind));
	}
	return type;
}

const substrait::Type &Carrier(const substrait::Type &type) {
	return type.user_defined().type_parameters(2).data_type();
}

void AppendUtf8(uint32_t code_point, std::string &out) {
	if (code_point < 0x80) {
		out += static_cast<char>(code_point);
	} else if (code_point < 0x800) {
		out += static_cast<char>(0xc0 | (code_point >> 6));
		out += static_cast<char>(0x80 | (code_point & 0x3f));
	} else if (code_point < 0x10000) {
		out += static_cast<char>(0xe0 | (code_point >> 12));
		out += static_cast<char>(0x80 | ((code_point >> 6) & 0x3f));
		out += static_cast<char>(0x80 | (code_point & 0x3f));
	} else {
		out += static_cast<char>(0xf0 | (code_point >> 18));
		out += static_cast<char>(0x80 | ((code_point >> 12) & 0x3f));
		out += static_cast<char>(0x80 | ((code_point >> 6) & 0x3f));
		out += static_cast<char>(0x80 | (code_point & 0x3f));
	}
}

// The members of an Enum's `{"members":[...]}` parameters, whichever JSON
// escapes the writer chose; the adapter and the connector API each write them.
duckdb::vector<std::string> EnumMembers(const std::string &parameters) {
	duckdb::vector<std::string> members;
	auto position = parameters.find('[');
	while (position != std::string::npos) {
		position = parameters.find('"', position);
		if (position == std::string::npos) {
			break;
		}
		std::string member;
		for (position++; position < parameters.size() && parameters[position] != '"'; position++) {
			if (parameters[position] != '\\') {
				member += parameters[position];
				continue;
			}
			auto escaped = parameters[++position];
			switch (escaped) {
			case 'b':
				member += '\b';
				break;
			case 'f':
				member += '\f';
				break;
			case 'n':
				member += '\n';
				break;
			case 'r':
				member += '\r';
				break;
			case 't':
				member += '\t';
				break;
			case 'u': {
				uint32_t code_point = std::stoul(parameters.substr(position + 1, 4), nullptr, 16);
				position += 4;
				// A surrogate pair encodes a code point above the Basic
				// Multilingual Plane.
				if (code_point >= 0xd800 && code_point < 0xdc00 && parameters.compare(position + 1, 2, "\\u") == 0) {
					uint32_t low = std::stoul(parameters.substr(position + 3, 4), nullptr, 16);
					code_point = 0x10000 + ((code_point - 0xd800) << 10) + (low - 0xdc00);
					position += 6;
				}
				AppendUtf8(code_point, member);
				break;
			}
			default:
				// `"`, `\`, and `/` stand for themselves.
				member += escaped;
			}
		}
		members.push_back(member);
		position++;
	}
	return members;
}

duckdb::LogicalType MapType(const substrait::Type &type, NameCursor &names, bool top_level) {
	using duckdb::LogicalType;
	switch (type.kind_case()) {
	case substrait::Type::kBool:
		return LogicalType::BOOLEAN;
	case substrait::Type::kI8:
		return LogicalType::TINYINT;
	case substrait::Type::kI16:
		return LogicalType::SMALLINT;
	case substrait::Type::kI32:
		return LogicalType::INTEGER;
	case substrait::Type::kI64:
		return LogicalType::BIGINT;
	case substrait::Type::kFp32:
		return LogicalType::FLOAT;
	case substrait::Type::kFp64:
		return LogicalType::DOUBLE;
	case substrait::Type::kDecimal:
		// DuckDB decimals have no negative scale. The scan rescales such a
		// column to DOUBLE, which it can do only for a column of its own.
		if (type.decimal().scale() < 0) {
			if (!top_level) {
				throw duckdb::NotImplementedException(
				    "the DuckDB adapter reads a negative-scale decimal only as a column of its own");
			}
			return LogicalType::DOUBLE;
		}
		return LogicalType::DECIMAL(type.decimal().precision(), type.decimal().scale());
	case substrait::Type::kString:
		return LogicalType::VARCHAR;
	case substrait::Type::kBinary:
		return LogicalType::BLOB;
	case substrait::Type::kDate:
		return LogicalType::DATE;
	case substrait::Type::kPrecisionTime:
		return LogicalType::TIME;
	case substrait::Type::kPrecisionTimestamp:
		return LogicalType::TIMESTAMP;
	case substrait::Type::kPrecisionTimestampTz:
		return LogicalType::TIMESTAMP_TZ;
	case substrait::Type::kIntervalDay:
	case substrait::Type::kIntervalCompound:
		return LogicalType::INTERVAL;
	case substrait::Type::kList:
		return LogicalType::LIST(MapType(type.list().type(), names, false));
	case substrait::Type::kMap: {
		auto key = MapType(type.map().key(), names, false);
		return LogicalType::MAP(key, MapType(type.map().value(), names, false));
	}
	case substrait::Type::kStruct: {
		duckdb::child_list_t<LogicalType> children;
		for (auto &child : type.struct_().types()) {
			auto name = *names++;
			children.emplace_back(name, MapType(child, names, false));
		}
		return LogicalType::STRUCT(std::move(children));
	}
	case substrait::Type::kUserDefined: {
		auto name = LogicalName(type);
		if (name == "uuid") {
			return LogicalType::UUID;
		}
		if (name == "json") {
			return LogicalType::JSON();
		}
		if (name == "year") {
			return LogicalType::SMALLINT;
		}
		if (name == "timetz") {
			return LogicalType::VARCHAR;
		}
		if (name == "enum") {
			auto members = EnumMembers(type.user_defined().type_parameters(1).string());
			duckdb::Vector labels(LogicalType::VARCHAR, members.size());
			auto data = duckdb::FlatVector::GetData<duckdb::string_t>(labels);
			for (duckdb::idx_t i = 0; i < members.size(); i++) {
				data[i] = duckdb::StringVector::AddString(labels, members[i]);
			}
			return LogicalType::ENUM(labels, members.size());
		}
		throw duckdb::NotImplementedException("Analytics type %s is unknown to the DuckDB adapter", name);
	}
	default:
		throw duckdb::NotImplementedException("a Substrait type outside the Analytics types");
	}
}

// The Arrow format string of the storage type that encodes an Analytics type.
std::string StorageFormat(const substrait::Type &type) {
	switch (type.kind_case()) {
	case substrait::Type::kBool:
		return "b";
	case substrait::Type::kI8:
		return "c";
	case substrait::Type::kI16:
		return "s";
	case substrait::Type::kI32:
		return "i";
	case substrait::Type::kI64:
		return "l";
	case substrait::Type::kFp32:
		return "f";
	case substrait::Type::kFp64:
		return "g";
	case substrait::Type::kDecimal:
		return "d:" + std::to_string(type.decimal().precision()) + "," + std::to_string(type.decimal().scale());
	case substrait::Type::kString:
		return "u";
	case substrait::Type::kBinary:
		return "z";
	case substrait::Type::kDate:
		return "tdD";
	case substrait::Type::kPrecisionTime:
		return "ttu";
	case substrait::Type::kPrecisionTimestamp:
		return "tsu:";
	case substrait::Type::kPrecisionTimestampTz:
		return "tsu:UTC";
	case substrait::Type::kIntervalDay:
		return "tDu";
	case substrait::Type::kIntervalCompound:
		return "tin";
	case substrait::Type::kList:
		return "+l";
	case substrait::Type::kMap:
		return "+m";
	case substrait::Type::kStruct:
		return "+s";
	case substrait::Type::kUserDefined:
		return StorageFormat(Carrier(type));
	default:
		return "";
	}
}

void CheckArrow(const ArrowSchema &schema, const substrait::Type &type, NameCursor &names, const std::string &path) {
	auto fail = [&](const std::string &what) {
		throw duckdb::IOException("ScalarDB Analytics read column %s with %s other than planned", path, what);
	};
	if (schema.format != StorageFormat(type)) {
		fail("storage type " + std::string(schema.format));
	}
	// A dictionary-encoded column is valid Arrow, but its format names the
	// index type, and DuckDB reads the dictionary's values.
	if (schema.dictionary) {
		fail("dictionary encoding");
	}
	if (((schema.flags & ARROW_FLAG_NULLABLE) != 0) != IsNullable(type)) {
		fail("nullability");
	}
	auto extension = LogicalName(type);
	duckdb::ArrowSchemaMetadata metadata(schema.metadata);
	auto received = metadata.GetOption(duckdb::ArrowSchemaMetadata::ARROW_EXTENSION_NAME);
	if (received != (extension.empty() ? "" : "scalardb.analytics." + extension)) {
		fail("extension " + received);
	}
	switch (type.kind_case()) {
	case substrait::Type::kList:
		CheckArrow(*schema.children[0], type.list().type(), names, path + "[]");
		break;
	case substrait::Type::kMap: {
		auto &entries = *schema.children[0];
		CheckArrow(*entries.children[0], type.map().key(), names, path + ".key");
		CheckArrow(*entries.children[1], type.map().value(), names, path + ".value");
		break;
	}
	case substrait::Type::kStruct:
		// DuckDB reads every child, so an extra field would make another
		// struct type.
		if (schema.n_children != type.struct_().types_size()) {
			fail(std::to_string(schema.n_children) + " fields");
		}
		for (int i = 0; i < type.struct_().types_size(); i++) {
			auto name = *names++;
			auto &child = *schema.children[i];
			if (name != child.name) {
				fail("field name " + std::string(child.name));
			}
			CheckArrow(child, type.struct_().types(i), names, path + "." + name);
		}
		break;
	default:
		break;
	}
}

// Appends a canonical description of a type without its nullability.
void Describe(const substrait::Type &type, std::string &out) {
	out += std::to_string(static_cast<int>(type.kind_case()));
	switch (type.kind_case()) {
	case substrait::Type::kDecimal:
		out += "(" + std::to_string(type.decimal().precision()) + "," + std::to_string(type.decimal().scale()) + ")";
		break;
	case substrait::Type::kPrecisionTime:
		out += "(" + std::to_string(type.precision_time().precision()) + ")";
		break;
	case substrait::Type::kPrecisionTimestamp:
		out += "(" + std::to_string(type.precision_timestamp().precision()) + ")";
		break;
	case substrait::Type::kPrecisionTimestampTz:
		out += "(" + std::to_string(type.precision_timestamp_tz().precision()) + ")";
		break;
	case substrait::Type::kList:
		out += "<";
		Describe(type.list().type(), out);
		out += IsNullable(type.list().type()) ? "?>" : ">";
		break;
	case substrait::Type::kMap:
		out += "<";
		Describe(type.map().key(), out);
		out += ",";
		Describe(type.map().value(), out);
		out += IsNullable(type.map().value()) ? "?>" : ">";
		break;
	case substrait::Type::kStruct:
		out += "<";
		for (auto &child : type.struct_().types()) {
			Describe(child, out);
			out += IsNullable(child) ? "?," : ",";
		}
		out += ">";
		break;
	case substrait::Type::kUserDefined:
		out += "<" + LogicalName(type) + ",";
		if (LogicalName(type) == "enum") {
			// Members, not their JSON spelling, identify an Enum; each is
			// prefixed with its length so that no label can mimic a boundary.
			for (auto &member : EnumMembers(type.user_defined().type_parameters(1).string())) {
				out += std::to_string(member.size()) + ":" + member;
			}
		} else {
			out += type.user_defined().type_parameters(1).string();
		}
		out += ",";
		Describe(Carrier(type), out);
		out += ">";
		break;
	default:
		break;
	}
}

size_t CountNestedNames(const substrait::Type &type) {
	switch (type.kind_case()) {
	case substrait::Type::kList:
		return CountNestedNames(type.list().type());
	case substrait::Type::kMap:
		return CountNestedNames(type.map().key()) + CountNestedNames(type.map().value());
	case substrait::Type::kStruct: {
		size_t count = 0;
		for (auto &child : type.struct_().types()) {
			count += 1 + CountNestedNames(child);
		}
		return count;
	}
	default:
		return 0;
	}
}

} // namespace

Field FieldFromMetadata(const std::string &name, const ScalardbAnalyticsConnectorAnalyticsType &type, bool nullable,
                        uint32_t logical_anchor) {
	Field field;
	field.name = name;
	field.type = Encode(type, nullable, logical_anchor, field.nested_names);
	return field;
}

std::vector<Field> FieldsOf(const substrait::NamedStruct &schema) {
	std::vector<Field> fields;
	int name = 0;
	for (auto &type : schema.struct_().types()) {
		Field field;
		field.name = schema.names(name++);
		field.type = type;
		for (size_t i = CountNestedNames(type); i > 0; i--) {
			field.nested_names.push_back(schema.names(name++));
		}
		fields.push_back(std::move(field));
	}
	return fields;
}

substrait::NamedStruct NamedStructOf(const std::vector<Field> &fields) {
	substrait::NamedStruct schema;
	auto &types = *schema.mutable_struct_();
	types.set_nullability(substrait::Type::NULLABILITY_REQUIRED);
	for (auto &field : fields) {
		schema.add_names(field.name);
		for (auto &nested : field.nested_names) {
			schema.add_names(nested);
		}
		*types.add_types() = field.type;
	}
	return schema;
}

bool SameType(const substrait::Type &left, const substrait::Type &right) {
	std::string a, b;
	Describe(left, a);
	Describe(right, b);
	return a == b;
}

bool IsNullable(const substrait::Type &type) {
	const google::protobuf::Reflection *reflection = type.GetReflection();
	const google::protobuf::OneofDescriptor *kind = type.GetDescriptor()->FindOneofByName("kind");
	auto field = reflection->GetOneofFieldDescriptor(type, kind);
	if (!field) {
		return false;
	}
	auto &message = reflection->GetMessage(type, field);
	auto nullability = message.GetDescriptor()->FindFieldByName("nullability");
	return nullability &&
	       message.GetReflection()->GetEnumValue(message, nullability) == substrait::Type::NULLABILITY_NULLABLE;
}

std::string LogicalName(const substrait::Type &type) {
	if (type.kind_case() != substrait::Type::kUserDefined || type.user_defined().type_parameters_size() != 3) {
		return "";
	}
	return type.user_defined().type_parameters(0).string();
}

duckdb::LogicalType DuckDBType(const Field &field) {
	auto names = field.nested_names.cbegin();
	return MapType(field.type, names, true);
}

void CheckArrowField(const ArrowSchema &schema, const Field &field) {
	if (field.name != schema.name) {
		throw duckdb::IOException("ScalarDB Analytics read column %s where %s was planned", schema.name, field.name);
	}
	auto names = field.nested_names.cbegin();
	CheckArrow(schema, field.type, names, field.name);
}

} // namespace scalardb_analytics_duckdb
