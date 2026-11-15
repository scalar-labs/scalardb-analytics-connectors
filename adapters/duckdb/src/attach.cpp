// Attaching Data Sources: the `scalardb_analytics_connectors` attach type, one Data
// Source per catalog, the `scalardb_analytics_connectors` secret type that holds one Data
// Source's connection options, and the pragma that attaches every Data Source
// listed in a definitions file.
#include <map>

#include <google/protobuf/struct.pb.h>
#include <google/protobuf/util/json_util.h>

#include "connector_api.hpp"
#include "duckdb/function/pragma_function.hpp"
#include "duckdb/main/attached_database.hpp"
#include "duckdb/main/connection.hpp"
#include "duckdb/main/database_manager.hpp"
#include "duckdb/main/settings.hpp"
#include "duckdb/main/extension/extension_loader.hpp"
#include "duckdb/main/secret/secret_manager.hpp"
#include "duckdb/parser/parsed_data/attach_info.hpp"
#include "duckdb/storage/storage_extension.hpp"
#include "register.hpp"
#include "settings.hpp"
#include "scalardb_analytics_duckdb.hpp"

namespace scalardb_analytics_duckdb {

namespace {

// The name of both the attach type and the secret type.
constexpr const char *TYPE_NAME = "scalardb_analytics_connectors";

// What a Connector declares for one connection option it accepts.
struct OptionDeclaration {
	// `string`, `integer`, or `boolean`.
	std::string value_type;
	// Whether the value is a credential, which a secret redacts.
	bool secret;
};

// The declared connection options, by Connector name and option key.
using OptionTypes = std::map<std::string, std::map<std::string, OptionDeclaration>>;

// A field of a static information object, which the document's schema
// requires.
const google::protobuf::Value &Field(const google::protobuf::Value &object, const std::string &key) {
	auto &fields = object.struct_value().fields();
	auto field = fields.find(key);
	if (field == fields.end()) {
		throw duckdb::InternalException("ScalarDB Analytics static information has an object without \"%s\"", key);
	}
	return field->second;
}

// Reads the declared options from the library's static information.
//
// DuckDB has no JSON parser in its public headers, and the protobuf the
// adapter links for Substrait parses JSON without a database.
OptionTypes ReadOptionTypes(const ScalardbAnalyticsConnectorApi &api) {
	google::protobuf::Value information;
	auto status = google::protobuf::util::JsonStringToMessage(Text(api.static_information()), &information);
	if (!status.ok()) {
		throw duckdb::InternalException("ScalarDB Analytics static information: %s", std::string(status.message()));
	}
	OptionTypes types;
	for (auto &connector : Field(information, "connectors").list_value().values()) {
		for (auto &handled : Field(connector, "connectors").list_value().values()) {
			auto &options = types[handled.string_value()];
			for (auto &option : Field(connector, "options").list_value().values()) {
				options[Field(option, "key").string_value()] = {Field(option, "value_type").string_value(),
				                                                 Field(option, "secret").bool_value()};
			}
		}
	}
	return types;
}

struct AdapterStorageInfo : public duckdb::StorageExtensionInfo {
	explicit AdapterStorageInfo(const ScalardbAnalyticsConnectorApi &api_p)
	    : api(api_p), option_types(ReadOptionTypes(api_p)) {
	}
	const ScalardbAnalyticsConnectorApi &api;
	const OptionTypes option_types;
};

// The text the connector API takes for an option value of the declared type.
//
// A value must have its declared type: a string as VARCHAR, an integer as an
// integer type within 64 bits, a boolean as BOOLEAN. No other value is
// converted, so the Connector receives exactly what was written. Messages
// name the option and types, never the value, which may be a credential.
std::string OptionText(const std::string &connector, const std::string &key, const std::string &type,
                       const duckdb::Value &value) {
	auto fail = [&](const std::string &expected) {
		throw duckdb::InvalidInputException("ScalarDB Analytics option \"%s\" of Connector %s must be %s, not %s", key,
		                                    connector, expected, value.type().ToString());
	};
	if (type == "string") {
		if (value.type().id() != duckdb::LogicalTypeId::VARCHAR) {
			fail("a string");
		}
		return duckdb::StringValue::Get(value);
	}
	if (type == "integer") {
		duckdb::Value integer;
		std::string error;
		if (!value.type().IsIntegral() ||
		    !value.DefaultTryCastAs(duckdb::LogicalType::BIGINT, integer, &error, true)) {
			fail("an integer within 64 bits");
		}
		return std::to_string(integer.GetValue<int64_t>());
	}
	if (type == "boolean") {
		if (value.type().id() != duckdb::LogicalTypeId::BOOLEAN) {
			fail("a boolean");
		}
		return value.GetValue<bool>() ? "true" : "false";
	}
	throw duckdb::NotImplementedException("ScalarDB Analytics option \"%s\" of Connector %s has value type %s", key,
	                                      connector, type);
}

// The Connector of a Data Source and its connection options, as DuckDB values.
struct ConnectionSource {
	std::string connector;
	std::map<std::string, duckdb::Value> options;
};

// Reads the Connector and the connection options from the secret an ATTACH
// names with its `SECRET` option.
//
// Every connection option comes from the secret, so the ATTACH takes no other
// option, and its path, which names the Connector without a secret, is empty
// or the secret's Connector. Messages name the secret, never a value.
ConnectionSource FromSecret(duckdb::ClientContext &context, const std::string &path,
                            const std::unordered_map<std::string, duckdb::Value> &options) {
	if (options.size() > 1) {
		throw duckdb::InvalidInputException(
		    "ScalarDB Analytics takes every connection option from the secret; the ATTACH takes no other option");
	}
	auto name = options.at("secret").ToString();
	auto entry = duckdb::SecretManager::Get(context).GetSecretByName(
	    duckdb::CatalogTransaction::GetSystemCatalogTransaction(context), name);
	if (!entry) {
		throw duckdb::InvalidInputException("ScalarDB Analytics found no secret named \"%s\"", name);
	}
	auto secret = dynamic_cast<const duckdb::KeyValueSecret *>(entry->secret.get());
	if (entry->secret->GetType() != TYPE_NAME || !secret) {
		throw duckdb::InvalidInputException("Secret \"%s\" is of type %s, not %s", name, entry->secret->GetType(),
		                                    TYPE_NAME);
	}
	auto &connector = secret->GetProvider();
	if (!path.empty() && path != connector) {
		throw duckdb::InvalidInputException("Secret \"%s\" holds a Data Source of Connector %s, not %s", name,
		                                    connector, path);
	}
	return {connector, {secret->secret_map.begin(), secret->secret_map.end()}};
}

// `CREATE SECRET (TYPE scalardb_analytics_connectors, PROVIDER <connector>, <option> <value>, ...)`
// holds the connection options of one Data Source of the Connector the
// provider names. DuckDB has already cast each value to the type the
// Connector declares; the options the Connector declares secret are redacted.
duckdb::unique_ptr<duckdb::BaseSecret> CreateSecret(duckdb::ClientContext &context,
                                                    duckdb::CreateSecretInput &input) {
	auto extension = duckdb::StorageExtension::Find(duckdb::DBConfig::GetConfig(context), TYPE_NAME);
	auto &declared = static_cast<AdapterStorageInfo &>(*extension->storage_info).option_types.at(input.provider);
	auto secret = duckdb::make_uniq<duckdb::KeyValueSecret>(input.scope, input.type, input.provider, input.name);
	for (auto &option : input.options) {
		secret->secret_map[option.first] = option.second;
		if (declared.at(option.first).secret) {
			secret->redact_keys.insert(option.first);
		}
	}
	return std::move(secret);
}

// `ATTACH '<connector>' AS <name> (TYPE scalardb_analytics_connectors, <option> <value>, ...)`
// opens a Data Source of the Connector the path names; every other option
// is a connection option of the Connector, of the type the Connector
// declares for it. With `SECRET <secret>` instead, the Connector and every
// connection option come from that secret.
duckdb::unique_ptr<duckdb::Catalog> Attach(duckdb::optional_ptr<duckdb::StorageExtensionInfo> storage_info,
                                           duckdb::ClientContext &context, duckdb::AttachedDatabase &database,
                                           const std::string &name, duckdb::AttachInfo &info,
                                           duckdb::AttachOptions &options) {
	// A Data Source is external access, which a database may forbid.
	if (!duckdb::Settings::Get<duckdb::EnableExternalAccessSetting>(context)) {
		throw duckdb::PermissionException("ScalarDB Analytics Data Sources are disabled by enable_external_access");
	}
	auto &storage = static_cast<AdapterStorageInfo &>(*storage_info);
	auto &api = storage.api;
	auto source = options.options.count("secret")
	                  ? FromSecret(context, info.path, options.options)
	                  : ConnectionSource {info.path, {options.options.begin(), options.options.end()}};
	auto declared = storage.option_types.find(source.connector);
	if (declared == storage.option_types.end()) {
		throw duckdb::InvalidInputException("ScalarDB Analytics has no Connector %s", source.connector);
	}
	std::vector<std::pair<std::string, std::string>> connection;
	for (auto &option : source.options) {
		auto type = declared->second.find(option.first);
		if (type == declared->second.end()) {
			throw duckdb::InvalidInputException("ScalarDB Analytics Connector %s does not accept option \"%s\"",
			                                    source.connector, option.first);
		}
		connection.emplace_back(option.first,
		                        OptionText(source.connector, option.first, type->second.value_type, option.second));
	}
	options.options.clear();
	auto data_source =
	    std::make_shared<DataSource>(api, source.connector, connection, Milliseconds(context, CONNECTION_TIMEOUT));
	return CreateCatalog(database, data_source,
	                     ConnectorCatalogSource(data_source, Milliseconds(context, METADATA_TIMEOUT)));
}

duckdb::unique_ptr<duckdb::TransactionManager>
CreateTransactions(duckdb::optional_ptr<duckdb::StorageExtensionInfo>, duckdb::AttachedDatabase &database,
                   duckdb::Catalog &) {
	return CreateTransactionManager(database);
}

// The ATTACH options DuckDB consumes itself; a Connector never receives them.
const char *const RESERVED_OPTIONS[] = {"type",     "read_only", "readonly",      "read_write",
                                        "readwrite", "hidden",   "default_table", "recovery_mode"};

// One Data Source of a definitions file.
struct Definition {
	std::string name;
	std::string connector;
	std::vector<std::pair<std::string, duckdb::Value>> options;
};

// Runs a query and returns its rows, or throws naming the definitions file.
duckdb::unique_ptr<duckdb::MaterializedQueryResult> Query(duckdb::Connection &connection, const std::string &path,
                                                          const std::string &sql, duckdb::Value parameter) {
	auto statement = connection.Prepare(sql);
	if (statement->HasError()) {
		throw duckdb::InternalException("ScalarDB Analytics definitions query: %s", statement->GetError());
	}
	duckdb::vector<duckdb::Value> values {std::move(parameter)};
	auto result = statement->Execute(values, false);
	if (result->HasError()) {
		throw duckdb::IOException("ScalarDB Analytics could not read the definitions file %s: %s", path,
		                          result->GetError());
	}
	return duckdb::unique_ptr_cast<duckdb::QueryResult, duckdb::MaterializedQueryResult>(std::move(result));
}

// Reads a definitions file: a JSON array of objects with a non-empty string
// `name` and `connector`, and an optional `options` object whose values are
// strings, integers, or booleans. Anything else is rejected rather than read
// as something else, and
// error messages name keys and JSON types only, never values, which may be
// credentials.
std::vector<Definition> ReadDefinitions(duckdb::Connection &connection, const std::string &path) {
	auto entries = Query(connection, path, "SELECT json FROM read_json_objects(?, format = 'array')", path);
	std::vector<Definition> definitions;
	for (duckdb::idx_t i = 0; i < entries->RowCount(); i++) {
		auto entry = entries->GetValue(0, i);
		auto fail = [&](const std::string &what) {
			throw duckdb::InvalidInputException("ScalarDB Analytics definitions file %s, entry %llu: %s", path, i + 1,
			                                    what);
		};
		auto kind = Query(connection, path, "SELECT json_type(?::JSON)", entry);
		if (kind->GetValue(0, 0).ToString() != "OBJECT") {
			fail("an entry must be an object, not " + kind->GetValue(0, 0).ToString());
		}
		auto keys = Query(connection, path, "SELECT key, type, value->>'$' FROM json_each(?::JSON)", entry);
		Definition definition;
		bool named = false, connected = false;
		for (auto &key : *keys) {
			auto name = key.GetValue<std::string>(0);
			auto type = key.GetValue<std::string>(1);
			if (name == "name" || name == "connector") {
				if (type != "VARCHAR" || key.IsNull(2) || key.GetValue<std::string>(2).empty()) {
					fail("\"" + name + "\" must be a non-empty string, not " + type);
				}
				(name == "name" ? definition.name : definition.connector) = key.GetValue<std::string>(2);
				(name == "name" ? named : connected) = true;
			} else if (name == "options") {
				if (type != "OBJECT") {
					fail("\"options\" must be an object, not " + type);
				}
			} else {
				fail("unknown key \"" + name + "\"; an entry has \"name\", \"connector\", and \"options\"");
			}
		}
		if (!named || !connected) {
			fail("an entry must be an object with \"name\" and \"connector\"");
		}
		auto options = Query(connection, path, "SELECT key, type, value->>'$' FROM json_each(?::JSON, '$.options')",
		                     entry);
		for (auto &option : *options) {
			auto key = option.GetValue<std::string>(0);
			auto type = option.GetValue<std::string>(1);
			for (auto reserved : RESERVED_OPTIONS) {
				if (duckdb::StringUtil::CIEquals(key, reserved)) {
					fail("option \"" + key + "\" is an ATTACH option of DuckDB, not of the Connector");
				}
			}
			// A JSON string, integer, or boolean becomes the DuckDB value an
			// ATTACH option of that type has; the Connector's declared type
			// is checked when the entry is attached.
			auto text = option.IsNull(2) ? std::string() : option.GetValue<std::string>(2);
			duckdb::Value value;
			if (type == "VARCHAR") {
				value = duckdb::Value(text);
			} else if (type == "BIGINT") {
				value = duckdb::Value(text).DefaultCastAs(duckdb::LogicalType::BIGINT);
			} else if (type == "UBIGINT") {
				value = duckdb::Value(text).DefaultCastAs(duckdb::LogicalType::UBIGINT);
			} else if (type == "BOOLEAN") {
				value = duckdb::Value::BOOLEAN(text == "true");
			} else {
				fail("option \"" + key + "\" must be a string, an integer, or a boolean, not " + type);
			}
			definition.options.emplace_back(key, std::move(value));
		}
		for (auto &other : definitions) {
			if (duckdb::StringUtil::CIEquals(other.name, definition.name)) {
				fail("the name \"" + definition.name + "\" is already used by another entry");
			}
		}
		definitions.push_back(std::move(definition));
	}
	if (definitions.empty()) {
		throw duckdb::InvalidInputException("ScalarDB Analytics definitions file %s lists no Data Source", path);
	}
	return definitions;
}

// `PRAGMA scalardb_analytics_attach('<file>')` attaches every Data Source a
// JSON definitions file lists:
//
//     [{"name": "pg", "connector": "postgresql", "options": {"host": "..."}}]
//
// Each entry is attached as an ATTACH statement does, so each Data Source is
// its own catalog, and either every entry is attached or none is. It
// attaches directly rather than running generated ATTACH statements, whose
// text, credentials included, DuckDB's query log would record.
void AttachAll(duckdb::ClientContext &context, const duckdb::FunctionParameters &parameters) {
	auto path = parameters.values[0].ToString();
	duckdb::Connection connection(*context.db);
	auto definitions = ReadDefinitions(connection, path);
	auto &config = duckdb::DBConfig::GetConfig(context);
	auto &databases = duckdb::DatabaseManager::Get(context);
	std::vector<std::string> attached;
	try {
		for (auto &definition : definitions) {
			duckdb::AttachInfo info;
			info.name = definition.name;
			info.path = definition.connector;
			info.options["type"] = duckdb::Value(TYPE_NAME);
			for (auto &option : definition.options) {
				info.options[option.first] = option.second;
			}
			duckdb::AttachOptions attach_options(info.options, config.options.access_mode);
			databases.AttachDatabase(context, info, attach_options);
			attached.push_back(definition.name);
		}
	} catch (...) {
		for (auto &name : attached) {
			databases.DetachDatabase(context, name, duckdb::OnEntryNotFound::RETURN_NULL);
		}
		throw;
	}
}

} // namespace

void RegisterWith(duckdb::ExtensionLoader &loader, const ScalardbAnalyticsConnectorApi &api) {
	auto &config = duckdb::DBConfig::GetConfig(loader.GetDatabaseInstance());
	AddSettings(config);
	auto storage = duckdb::make_shared_ptr<duckdb::StorageExtension>();
	storage->attach = Attach;
	storage->create_transaction_manager = CreateTransactions;
	auto info = duckdb::make_shared_ptr<AdapterStorageInfo>(api);
	storage->storage_info = info;
	duckdb::StorageExtension::Register(config, TYPE_NAME, std::move(storage));

	duckdb::SecretType secret_type;
	secret_type.name = TYPE_NAME;
	secret_type.deserializer = duckdb::KeyValueSecret::Deserialize<duckdb::KeyValueSecret>;
	secret_type.extension = "scalardb_analytics_connectors";
	loader.RegisterSecretType(secret_type);
	// One provider per Connector, taking the options it declares with their types.
	for (auto &connector : info->option_types) {
		duckdb::CreateSecretFunction function {TYPE_NAME, connector.first, CreateSecret, {}};
		for (auto &option : connector.second) {
			auto &type = option.second.value_type;
			function.named_parameters[option.first] = type == "integer"   ? duckdb::LogicalType::BIGINT
			                                          : type == "boolean" ? duckdb::LogicalType::BOOLEAN
			                                                              : duckdb::LogicalType::VARCHAR;
		}
		loader.RegisterFunction(function);
	}
	loader.RegisterFunction(
	    duckdb::PragmaFunction::PragmaCall("scalardb_analytics_attach", AttachAll, {duckdb::LogicalType::VARCHAR}));
}

void Register(duckdb::DatabaseInstance &database, const ScalardbAnalyticsConnectorApi &api) {
	duckdb::ExtensionLoader loader(database, "scalardb_analytics_connectors");
	RegisterWith(loader, api);
}

} // namespace scalardb_analytics_duckdb
