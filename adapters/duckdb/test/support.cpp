#include "support.hpp"

#include <atomic>
#include <cstdlib>
#include <map>
#include <mutex>

#include "catch2/catch_test_macros.hpp"
#include "duckdb/main/attached_database.hpp"
#include "duckdb/storage/storage_extension.hpp"

namespace test_support {

using scalardb_analytics_duckdb::Names;

const ScalardbAnalyticsConnectorApi &Api() {
	static std::once_flag initialized;
	auto &api = *scalardb_analytics_connector_api();
	std::call_once(initialized, [&api]() {
		ScalardbAnalyticsConnectorLibrarySettings settings {std::getenv("ADBC_DRIVER_DIRECTORY")};
		auto error = api.library_initialize(&settings);
		REQUIRE(error == nullptr);
	});
	return api;
}

Database::Database() : database(nullptr), connection(database) {
	scalardb_analytics_duckdb::Register(*database.instance, Api());
}

std::vector<std::vector<std::string>> Database::Rows(const std::string &sql) {
	auto result = connection.Query(sql);
	INFO(sql);
	INFO((result->HasError() ? result->GetError() : std::string()));
	REQUIRE_FALSE(result->HasError());
	std::vector<std::vector<std::string>> rows;
	for (duckdb::idx_t row = 0; row < result->RowCount(); row++) {
		std::vector<std::string> values;
		for (duckdb::idx_t column = 0; column < result->ColumnCount(); column++) {
			values.push_back(result->GetValue(column, row).ToString());
		}
		rows.push_back(values);
	}
	return rows;
}

std::string Database::Error(const std::string &sql) {
	auto result = connection.Query(sql);
	INFO(sql);
	REQUIRE(result->HasError());
	return result->GetError();
}

std::string Database::Explain(const std::string &sql) {
	auto rows = Rows("EXPLAIN " + sql);
	return rows.at(0).at(1);
}

namespace {

ScalardbAnalyticsConnectorString String(const std::string &value) {
	return {value.c_str(), value.size()};
}

// One table's metadata structures with the storage they point into.
struct FixtureTable {
	FixtureTable(const Names &path, const std::vector<FixtureColumn> &columns) : names(path) {
		for (auto &name : names) {
			name_strings.push_back(String(name));
		}
		for (auto &column : columns) {
			column_names.push_back(column.name);
		}
		for (size_t i = 0; i < columns.size(); i++) {
			ScalardbAnalyticsConnectorColumn column {};
			column.name = String(column_names[i]);
			column.ordinal = static_cast<uint32_t>(i + 1);
			column.nullable = columns[i].nullable ? ScalardbAnalyticsConnectorBool_TRUE : ScalardbAnalyticsConnectorBool_FALSE;
			column.source_id.state = ScalardbAnalyticsConnectorReportedState_NOT_OFFERED;
			column.type_state = ScalardbAnalyticsConnectorColumnTypeState_MAPPED;
			column.analytics_type.kind = columns[i].kind;
			auto &type = column.analytics_type;
			switch (columns[i].kind) {
			case ScalardbAnalyticsConnectorAnalyticsTypeKind_DECIMAL:
				type.decimal_precision = 10;
				type.decimal_scale = columns[i].decimal_scale;
				break;
			case ScalardbAnalyticsConnectorAnalyticsTypeKind_ENUM:
				type.members = members;
				type.member_count = 2;
				break;
			case ScalardbAnalyticsConnectorAnalyticsTypeKind_ARRAY:
				type.element_type = &text_type;
				type.element_nullable = ScalardbAnalyticsConnectorBool_TRUE;
				break;
			case ScalardbAnalyticsConnectorAnalyticsTypeKind_MAP:
				type.key_type = &text_type;
				type.value_type = &big_int_type;
				type.value_nullable = ScalardbAnalyticsConnectorBool_TRUE;
				break;
			case ScalardbAnalyticsConnectorAnalyticsTypeKind_STRUCT:
				type.fields = &struct_field;
				type.field_count = 1;
				break;
			default:
				break;
			}
			this->columns.push_back(column);
		}
		metadata = {};
		metadata.table = {name_strings.data(), name_strings.size()};
		metadata.kind = ScalardbAnalyticsConnectorTableKind_TABLE;
		metadata.source_id.state = ScalardbAnalyticsConnectorReportedState_NOT_OFFERED;
		metadata.columns = this->columns.data();
		metadata.column_count = this->columns.size();
		metadata.constraints_state = ScalardbAnalyticsConnectorReportedState_NOT_OFFERED;
	}

	Names names;
	std::vector<ScalardbAnalyticsConnectorString> name_strings;
	std::vector<std::string> column_names;
	std::vector<ScalardbAnalyticsConnectorColumn> columns;
	ScalardbAnalyticsConnectorString members[2] = {{"pending", 7}, {"done", 4}};
	// The element, key, value, and field types of nested columns.
	ScalardbAnalyticsConnectorAnalyticsType text_type {ScalardbAnalyticsConnectorAnalyticsTypeKind_TEXT};
	ScalardbAnalyticsConnectorAnalyticsType big_int_type {ScalardbAnalyticsConnectorAnalyticsTypeKind_BIG_INT};
	ScalardbAnalyticsConnectorAnalyticsField struct_field {{"f", 1}, &big_int_type, ScalardbAnalyticsConnectorBool_TRUE};
	ScalardbAnalyticsConnectorTableMetadata metadata;
};

using FixtureTables = std::vector<std::pair<Names, std::vector<FixtureColumn>>>;

// A catalog source that serves fixed table metadata, as one holding
// registered metadata would.
class FixtureSource : public scalardb_analytics_duckdb::CatalogSource {
public:
	explicit FixtureSource(FixtureTables tables_p) : tables(std::move(tables_p)) {
	}

	std::vector<Names> Namespaces() override {
		std::vector<Names> result;
		for (auto &table : tables) {
			Names path(table.first.begin(), table.first.end() - 1);
			if (std::find(result.begin(), result.end(), path) == result.end()) {
				result.push_back(path);
			}
		}
		return result;
	}

	std::unique_ptr<scalardb_analytics_duckdb::NamespaceMetadata> Namespace(const Names &namespace_path) override {
		auto namespaces = Namespaces();
		if (std::find(namespaces.begin(), namespaces.end(), namespace_path) == namespaces.end()) {
			return nullptr;
		}
		return std::make_unique<scalardb_analytics_duckdb::NamespaceMetadata>(
		    scalardb_analytics_duckdb::NamespaceMetadata {namespace_path});
	}

	std::vector<Names> Tables(const Names &namespace_path) override {
		std::vector<Names> result;
		for (auto &table : tables) {
			if (Names(table.first.begin(), table.first.end() - 1) == namespace_path &&
			    std::find(result.begin(), result.end(), table.first) == result.end()) {
				result.push_back(table.first);
			}
		}
		return result;
	}

	// A path given more than once is described by its entries in turn, the
	// last one from then on.
	std::unique_ptr<scalardb_analytics_duckdb::TableMetadata> Table(const Names &path) override {
		const std::vector<FixtureColumn> *columns = nullptr;
		size_t seen = 0;
		auto turn = described[path]++;
		for (auto &table : tables) {
			if (table.first == path && seen++ <= turn) {
				columns = &table.second;
			}
		}
		if (!columns) {
			return nullptr;
		}
		auto fixture = std::make_shared<FixtureTable>(path, *columns);
		return std::make_unique<scalardb_analytics_duckdb::TableMetadata>(fixture->metadata, [fixture]() {});
	}

private:
	FixtureTables tables;
	// How many times each path was described.
	std::map<Names, size_t> described;
};

struct FixtureInfo : public duckdb::StorageExtensionInfo {
	explicit FixtureInfo(FixtureTables tables_p) : tables(std::move(tables_p)) {
	}
	FixtureTables tables;
};

duckdb::unique_ptr<duckdb::Catalog> AttachFixtureCatalog(duckdb::optional_ptr<duckdb::StorageExtensionInfo> info,
                                                         duckdb::ClientContext &, duckdb::AttachedDatabase &database,
                                                         const std::string &, duckdb::AttachInfo &,
                                                         duckdb::AttachOptions &options) {
	std::vector<std::pair<std::string, std::string>> connection;
	for (auto &option : options.options) {
		connection.emplace_back(option.first, option.second.ToString());
	}
	options.options.clear();
	auto data_source = std::make_shared<scalardb_analytics_duckdb::DataSource>(Api(), "test", connection, 0);
	auto &tables = static_cast<FixtureInfo &>(*info).tables;
	return scalardb_analytics_duckdb::CreateCatalog(database, data_source, std::make_unique<FixtureSource>(tables));
}

duckdb::unique_ptr<duckdb::TransactionManager> FixtureTransactions(duckdb::optional_ptr<duckdb::StorageExtensionInfo>,
                                                                   duckdb::AttachedDatabase &database,
                                                                   duckdb::Catalog &) {
	return scalardb_analytics_duckdb::CreateTransactionManager(database);
}

} // namespace

void AttachFixture(Database &database, const std::string &alias, const FixtureTables &tables) {
	auto storage = duckdb::make_shared_ptr<duckdb::StorageExtension>();
	storage->attach = AttachFixtureCatalog;
	storage->create_transaction_manager = FixtureTransactions;
	storage->storage_info = duckdb::make_shared_ptr<FixtureInfo>(tables);
	auto type = "fixture_" + alias;
	duckdb::StorageExtension::Register(duckdb::DBConfig::GetConfig(*database.database.instance), type,
	                                   std::move(storage));
	database.Rows("ATTACH 'test' AS " + alias + " (TYPE " + type + ", label '" + UniqueLabel() + "')");
}

std::vector<FixtureColumn> ValueColumn() {
	return {{"value", ScalardbAnalyticsConnectorAnalyticsTypeKind_BIG_INT, false}};
}

std::string UniqueLabel() {
	static std::atomic<int> next {0};
	return "duckdb-adapter-" + std::to_string(next++);
}

} // namespace test_support
