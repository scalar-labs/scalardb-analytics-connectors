#include "catalog.hpp"

#include "duckdb/parser/constraints/not_null_constraint.hpp"
#include "duckdb/parser/parsed_data/create_schema_info.hpp"
#include "duckdb/parser/parsed_data/create_table_info.hpp"
#include "duckdb/storage/database_size.hpp"
#include "duckdb/transaction/transaction.hpp"
#include "duckdb/transaction/transaction_manager.hpp"
#include "connector_api.hpp"
#include "scan.hpp"

namespace scalardb_analytics_duckdb {

namespace {

[[noreturn]] void ReadOnly() {
	throw duckdb::PermissionException("ScalarDB Analytics catalogs are read-only");
}

// Finds a schema or table by name as DuckDB resolves identifiers: the exact
// name, or else the one name that differs only in case.
template <class T>
typename std::map<std::string, T>::iterator FindName(std::map<std::string, T> &entries, const std::string &name) {
	auto exact = entries.find(name);
	if (exact != entries.end()) {
		return exact;
	}
	auto found = entries.end();
	for (auto entry = entries.begin(); entry != entries.end(); entry++) {
		if (duckdb::StringUtil::CIEquals(entry->first, name)) {
			if (found != entries.end()) {
				throw duckdb::CatalogException("\"%s\" is ambiguous: \"%s\" and \"%s\" differ only in case", name,
				                               found->first, entry->first);
			}
			found = entry;
		}
	}
	return found;
}

// The schema name of a namespace: its name path joined with dots.
//
// A name containing a dot or a double quote is written as a DuckDB quoted
// identifier, so that distinct name paths, such as `a.b` and `a`, `b`, never
// share a schema name.
std::string SchemaName(const Names &namespace_path) {
	std::string name;
	for (auto &component : namespace_path) {
		if (!name.empty()) {
			name += ".";
		}
		if (component.find_first_of(".\"") == std::string::npos) {
			name += component;
			continue;
		}
		name += "\"";
		for (auto character : component) {
			name += character == '"' ? std::string("\"\"") : std::string(1, character);
		}
		name += "\"";
	}
	return name;
}

// The name path a schema name stands for, if it is one `SchemaName` writes.
bool SchemaPath(const std::string &name, Names &namespace_path) {
	namespace_path.clear();
	size_t position = 0;
	while (position <= name.size()) {
		std::string component;
		if (position < name.size() && name[position] == '"') {
			for (position++;; position++) {
				if (position >= name.size()) {
					return false;
				}
				if (name[position] == '"') {
					if (position + 1 < name.size() && name[position + 1] == '"') {
						component += '"';
						position++;
						continue;
					}
					position++;
					break;
				}
				component += name[position];
			}
		} else {
			auto end = name.find_first_of(".\"", position);
			component = name.substr(position, end == std::string::npos ? std::string::npos : end - position);
			position = end == std::string::npos ? name.size() : end;
		}
		namespace_path.push_back(std::move(component));
		if (position == name.size()) {
			break;
		}
		if (name[position] != '.') {
			return false;
		}
		position++;
	}
	// Only the spelling `SchemaName` writes stands for the path, and names are
	// never empty.
	for (auto &component : namespace_path) {
		if (component.empty()) {
			return false;
		}
	}
	return SchemaName(namespace_path) == name;
}

// Why a column has no Analytics type, with its Data Source type when read.
std::string UnmappedReason(const ScalardbAnalyticsConnectorColumn &column) {
	if (column.type_state == ScalardbAnalyticsConnectorColumnTypeState_UNREAD) {
		return "type not read: " + Text(column.unread_reason);
	}
	auto type = Text(column.data_source_type);
	switch (column.unsupported_reason) {
	case ScalardbAnalyticsConnectorUnsupportedReason_NOT_MAPPED:
		return type + " has no Analytics type";
	case ScalardbAnalyticsConnectorUnsupportedReason_MAPPING_NOT_IMPLEMENTED:
		return type + " is not mapped yet";
	case ScalardbAnalyticsConnectorUnsupportedReason_PARAMETER_OUT_OF_RANGE:
		return type + " has parameters outside the Analytics type";
	default:
		return type + " is unsupported";
	}
}

// The base schema of a table: its columns that have an Analytics type with a
// DuckDB type, in ordinal order. The other columns are not part of any plan;
// they are kept with the reason so that naming one can say why it cannot be
// read. A decimal wider than DuckDB's 38 digits has no DuckDB type.
std::shared_ptr<const TableShape> Shape(const ScalardbAnalyticsConnectorTableMetadata &metadata) {
	std::vector<const ScalardbAnalyticsConnectorColumn *> columns;
	for (size_t i = 0; i < metadata.column_count; i++) {
		columns.push_back(&metadata.columns[i]);
	}
	std::sort(columns.begin(), columns.end(),
	          [](const ScalardbAnalyticsConnectorColumn *a, const ScalardbAnalyticsConnectorColumn *b) {
		          return a->ordinal < b->ordinal;
	          });
	auto shape = std::make_shared<TableShape>();
	shape->binding = NamePath(metadata.table);
	for (auto column : columns) {
		if (column->type_state != ScalardbAnalyticsConnectorColumnTypeState_MAPPED) {
			shape->unmapped.emplace_back(Text(column->name), UnmappedReason(*column));
			continue;
		}
		auto &type = column->analytics_type;
		if (type.kind == ScalardbAnalyticsConnectorAnalyticsTypeKind_DECIMAL &&
		    type.decimal_precision > DUCKDB_DECIMAL_PRECISION) {
			shape->unmapped.emplace_back(Text(column->name),
			                             "Decimal(" + std::to_string(type.decimal_precision) + ", " +
			                                 std::to_string(type.decimal_scale) +
			                                 ") has no DuckDB type: DuckDB decimals hold at most 38 digits");
			continue;
		}
		shape->fields.push_back(FieldFromMetadata(Text(column->name), column->analytics_type,
		                                          column->nullable == ScalardbAnalyticsConnectorBool_TRUE,
		                                          LOGICAL_TYPE_ANCHOR));
	}
	return shape;
}

// The columns without an Analytics type and why, as one list.
std::string UnmappedColumns(const TableShape &shape) {
	std::string columns;
	for (auto &column : shape.unmapped) {
		columns += (columns.empty() ? "" : ", ") + column.first + " (" + column.second + ")";
	}
	return columns;
}

class AdapterTransaction : public duckdb::Transaction {
public:
	AdapterTransaction(duckdb::TransactionManager &manager, duckdb::ClientContext &context)
	    : duckdb::Transaction(manager, context) {
	}
};

// Transactions only satisfy DuckDB's bookkeeping: the catalog holds no
// transactional state.
class AdapterTransactionManager : public duckdb::TransactionManager {
public:
	explicit AdapterTransactionManager(duckdb::AttachedDatabase &database) : duckdb::TransactionManager(database) {
	}

	duckdb::Transaction &StartTransaction(duckdb::ClientContext &context) override {
		auto transaction = duckdb::make_uniq<AdapterTransaction>(*this, context);
		auto &result = *transaction;
		std::lock_guard<std::mutex> guard(lock);
		transactions[&result] = std::move(transaction);
		return result;
	}

	duckdb::ErrorData CommitTransaction(duckdb::ClientContext &context, duckdb::Transaction &transaction) override {
		std::lock_guard<std::mutex> guard(lock);
		transactions.erase(&transaction);
		return duckdb::ErrorData();
	}

	void RollbackTransaction(duckdb::Transaction &transaction) override {
		std::lock_guard<std::mutex> guard(lock);
		transactions.erase(&transaction);
	}

	void Checkpoint(duckdb::ClientContext &context, bool force) override {
	}

private:
	std::mutex lock;
	std::map<duckdb::Transaction *, duckdb::unique_ptr<duckdb::Transaction>> transactions;
};

} // namespace

AdapterTable::AdapterTable(duckdb::Catalog &catalog, duckdb::SchemaCatalogEntry &schema, duckdb::CreateTableInfo &info,
                           std::unique_ptr<TableMetadata> metadata_p, std::shared_ptr<const TableShape> shape_p)
    : duckdb::TableCatalogEntry(catalog, schema, info), metadata(std::move(metadata_p)), shape(std::move(shape_p)) {
}

duckdb::unique_ptr<duckdb::BaseStatistics> AdapterTable::GetStatistics(duckdb::ClientContext &context,
                                                                       duckdb::column_t column_id) {
	return nullptr;
}

duckdb::TableFunction AdapterTable::GetScanFunction(duckdb::ClientContext &context,
                                                    duckdb::unique_ptr<duckdb::FunctionData> &bind_data) {
	// A read names at least one column, so a table none of whose columns has
	// an Analytics type cannot be read.
	if (shape->fields.empty()) {
		throw duckdb::BinderException("ScalarDB Analytics cannot read %s: none of its columns has an Analytics type: %s",
		                              name, UnmappedColumns(*shape));
	}
	bind_data = ScanBindData(catalog.Cast<AdapterCatalog>().data_source, shape, *this);
	return ScanFunction();
}

duckdb::TableStorageInfo AdapterTable::GetStorageInfo(duckdb::ClientContext &context) {
	return duckdb::TableStorageInfo();
}

duckdb::virtual_column_map_t AdapterTable::GetVirtualColumns() const {
	return VirtualColumns(*shape);
}

duckdb::vector<duckdb::column_t> AdapterTable::GetRowIdColumns() const {
	return {};
}

duckdb::virtual_column_map_t VirtualColumns(const TableShape &shape) {
	duckdb::virtual_column_map_t columns;
	columns.insert(
	    std::make_pair(duckdb::COLUMN_IDENTIFIER_EMPTY, duckdb::TableColumn("", duckdb::LogicalType::BOOLEAN)));
	for (duckdb::idx_t i = 0; i < shape.unmapped.size(); i++) {
		// The type is never read; VARCHAR keeps the optimizer from folding a
		// condition on the column away.
		columns.insert(std::make_pair(duckdb::VIRTUAL_COLUMN_START + i,
		                              duckdb::TableColumn(shape.unmapped[i].first, duckdb::LogicalType::VARCHAR)));
	}
	return columns;
}

AdapterSchema::AdapterSchema(AdapterCatalog &catalog, duckdb::CreateSchemaInfo &info, Names namespace_path_p)
    : duckdb::SchemaCatalogEntry(catalog, info), namespace_path(std::move(namespace_path_p)) {
}

void AdapterSchema::LoadTableNames() {
	if (tables_listed) {
		return;
	}
	for (auto &path : catalog.Cast<AdapterCatalog>().source->Tables(namespace_path)) {
		table_paths[path.back()] = path;
	}
	tables_listed = true;
}

duckdb::optional_ptr<duckdb::CatalogEntry> AdapterSchema::Table(const std::string &lookup_name) {
	LoadTableNames();
	auto &source = *catalog.Cast<AdapterCatalog>().source;
	auto path = FindName(table_paths, lookup_name);
	std::unique_ptr<TableMetadata> metadata;
	if (path == table_paths.end()) {
		// Not listed: the table's own request tells whether it exists.
		if (missing.count(lookup_name)) {
			return nullptr;
		}
		auto table_path = namespace_path;
		table_path.push_back(lookup_name);
		metadata = source.Table(table_path);
		if (!metadata) {
			missing.insert(lookup_name);
			return nullptr;
		}
		path = table_paths.emplace(lookup_name, std::move(table_path)).first;
	}
	auto &name = path->first;
	auto loaded = tables.find(name);
	if (loaded != tables.end()) {
		if (!loaded->second->shape->stale) {
			return loaded->second.get();
		}
		// The Connector reported the shape stale: describe the table again.
		retired.push_back(std::move(loaded->second));
		tables.erase(loaded);
	}
	if (!metadata) {
		metadata = source.Table(path->second);
	}
	if (!metadata) {
		missing.insert(name);
		table_paths.erase(path);
		return nullptr;
	}
	auto shape = Shape(metadata->metadata);
	duckdb::CreateTableInfo info(*this, name);
	// The columns a query can name but not read are told where tables are
	// listed, since DuckDB lists only the readable ones.
	if (!shape->unmapped.empty()) {
		info.comment = duckdb::Value("Columns without an Analytics type: " + UnmappedColumns(*shape));
	}
	for (auto &field : shape->fields) {
		auto index = info.columns.LogicalColumnCount();
		info.columns.AddColumn(duckdb::ColumnDefinition(field.name, DuckDBType(field)));
		if (!IsNullable(field.type)) {
			info.constraints.push_back(duckdb::make_uniq<duckdb::NotNullConstraint>(duckdb::LogicalIndex(index)));
		}
	}
	auto table = duckdb::make_uniq<AdapterTable>(catalog, *this, info, std::move(metadata), std::move(shape));
	auto &result = *table;
	tables[name] = std::move(table);
	return &result;
}

void AdapterSchema::Scan(duckdb::ClientContext &context, duckdb::CatalogType type,
                         const std::function<void(duckdb::CatalogEntry &)> &callback) {
	Scan(type, callback);
}

void AdapterSchema::Scan(duckdb::CatalogType type, const std::function<void(duckdb::CatalogEntry &)> &callback) {
	if (type != duckdb::CatalogType::TABLE_ENTRY) {
		return;
	}
	std::lock_guard<std::mutex> guard(lock);
	LoadTableNames();
	for (auto &path : table_paths) {
		auto table = Table(path.first);
		if (table) {
			callback(*table);
		}
	}
}

duckdb::optional_ptr<duckdb::CatalogEntry> AdapterSchema::LookupEntry(duckdb::CatalogTransaction transaction,
                                                                     const duckdb::EntryLookupInfo &lookup_info) {
	if (lookup_info.GetCatalogType() != duckdb::CatalogType::TABLE_ENTRY) {
		return nullptr;
	}
	std::lock_guard<std::mutex> guard(lock);
	return Table(lookup_info.GetEntryName());
}

duckdb::optional_ptr<duckdb::CatalogEntry> AdapterSchema::CreateIndex(duckdb::CatalogTransaction,
                                                                     duckdb::CreateIndexInfo &,
                                                                     duckdb::TableCatalogEntry &) {
	ReadOnly();
}
duckdb::optional_ptr<duckdb::CatalogEntry> AdapterSchema::CreateFunction(duckdb::CatalogTransaction,
                                                                        duckdb::CreateFunctionInfo &) {
	ReadOnly();
}
duckdb::optional_ptr<duckdb::CatalogEntry> AdapterSchema::CreateTable(duckdb::CatalogTransaction,
                                                                     duckdb::BoundCreateTableInfo &) {
	ReadOnly();
}
duckdb::optional_ptr<duckdb::CatalogEntry> AdapterSchema::CreateView(duckdb::CatalogTransaction,
                                                                    duckdb::CreateViewInfo &) {
	ReadOnly();
}
duckdb::optional_ptr<duckdb::CatalogEntry> AdapterSchema::CreateSequence(duckdb::CatalogTransaction,
                                                                        duckdb::CreateSequenceInfo &) {
	ReadOnly();
}
duckdb::optional_ptr<duckdb::CatalogEntry> AdapterSchema::CreateTableFunction(duckdb::CatalogTransaction,
                                                                             duckdb::CreateTableFunctionInfo &) {
	ReadOnly();
}
duckdb::optional_ptr<duckdb::CatalogEntry> AdapterSchema::CreateCopyFunction(duckdb::CatalogTransaction,
                                                                            duckdb::CreateCopyFunctionInfo &) {
	ReadOnly();
}
duckdb::optional_ptr<duckdb::CatalogEntry> AdapterSchema::CreatePragmaFunction(duckdb::CatalogTransaction,
                                                                              duckdb::CreatePragmaFunctionInfo &) {
	ReadOnly();
}
duckdb::optional_ptr<duckdb::CatalogEntry> AdapterSchema::CreateCollation(duckdb::CatalogTransaction,
                                                                         duckdb::CreateCollationInfo &) {
	ReadOnly();
}
duckdb::optional_ptr<duckdb::CatalogEntry> AdapterSchema::CreateType(duckdb::CatalogTransaction,
                                                                    duckdb::CreateTypeInfo &) {
	ReadOnly();
}
void AdapterSchema::DropEntry(duckdb::ClientContext &, duckdb::DropInfo &) {
	ReadOnly();
}
void AdapterSchema::Alter(duckdb::CatalogTransaction, duckdb::AlterInfo &) {
	ReadOnly();
}

AdapterCatalog::AdapterCatalog(duckdb::AttachedDatabase &database, std::shared_ptr<DataSource> data_source_p,
                               std::unique_ptr<CatalogSource> source_p)
    : duckdb::Catalog(database), data_source(std::move(data_source_p)), source(std::move(source_p)) {
}

void AdapterCatalog::Initialize(bool load_builtin) {
}

std::string AdapterCatalog::GetCatalogType() {
	return "scalardb_analytics";
}

void AdapterCatalog::LoadSchemas() {
	if (schemas_loaded) {
		return;
	}
	for (auto &namespace_path : source->Namespaces()) {
		auto name = SchemaName(namespace_path);
		duckdb::CreateSchemaInfo info;
		info.schema = name;
		schemas[name] = duckdb::make_uniq<AdapterSchema>(*this, info, namespace_path);
	}
	schemas_loaded = true;
}

duckdb::optional_ptr<duckdb::SchemaCatalogEntry>
AdapterCatalog::LookupSchema(duckdb::CatalogTransaction transaction, const duckdb::EntryLookupInfo &schema_lookup,
                             duckdb::OnEntryNotFound if_not_found) {
	std::lock_guard<std::mutex> guard(lock);
	LoadSchemas();
	auto &lookup_name = schema_lookup.GetEntryName();
	auto schema = FindName(schemas, lookup_name);
	if (schema == schemas.end() && !missing_schemas.count(lookup_name)) {
		// Not listed: the namespace's own request tells whether it exists.
		Names namespace_path;
		if (SchemaPath(lookup_name, namespace_path) && source->Namespace(namespace_path)) {
			duckdb::CreateSchemaInfo info;
			info.schema = lookup_name;
			schema = schemas.emplace(lookup_name, duckdb::make_uniq<AdapterSchema>(*this, info, namespace_path)).first;
		} else {
			missing_schemas.insert(lookup_name);
		}
	}
	if (schema == schemas.end()) {
		if (if_not_found == duckdb::OnEntryNotFound::THROW_EXCEPTION) {
			throw duckdb::CatalogException("Schema \"%s\" does not exist in catalog \"%s\"",
			                               schema_lookup.GetEntryName(), GetName());
		}
		return nullptr;
	}
	return schema->second.get();
}

void AdapterCatalog::ScanSchemas(duckdb::ClientContext &context,
                                 std::function<void(duckdb::SchemaCatalogEntry &)> callback) {
	std::lock_guard<std::mutex> guard(lock);
	LoadSchemas();
	for (auto &schema : schemas) {
		callback(*schema.second);
	}
}

duckdb::optional_ptr<duckdb::CatalogEntry> AdapterCatalog::CreateSchema(duckdb::CatalogTransaction,
                                                                       duckdb::CreateSchemaInfo &) {
	ReadOnly();
}
duckdb::PhysicalOperator &AdapterCatalog::PlanCreateTableAs(duckdb::ClientContext &, duckdb::PhysicalPlanGenerator &,
                                                            duckdb::LogicalCreateTable &, duckdb::PhysicalOperator &) {
	ReadOnly();
}
duckdb::PhysicalOperator &AdapterCatalog::PlanInsert(duckdb::ClientContext &, duckdb::PhysicalPlanGenerator &,
                                                     duckdb::LogicalInsert &,
                                                     duckdb::optional_ptr<duckdb::PhysicalOperator>) {
	ReadOnly();
}
duckdb::PhysicalOperator &AdapterCatalog::PlanDelete(duckdb::ClientContext &, duckdb::PhysicalPlanGenerator &,
                                                     duckdb::LogicalDelete &, duckdb::PhysicalOperator &) {
	ReadOnly();
}
duckdb::PhysicalOperator &AdapterCatalog::PlanUpdate(duckdb::ClientContext &, duckdb::PhysicalPlanGenerator &,
                                                     duckdb::LogicalUpdate &, duckdb::PhysicalOperator &) {
	ReadOnly();
}
void AdapterCatalog::DropSchema(duckdb::ClientContext &, duckdb::DropInfo &) {
	ReadOnly();
}

duckdb::DatabaseSize AdapterCatalog::GetDatabaseSize(duckdb::ClientContext &context) {
	return duckdb::DatabaseSize();
}

bool AdapterCatalog::InMemory() {
	return false;
}

std::string AdapterCatalog::GetDBPath() {
	return "";
}

duckdb::unique_ptr<duckdb::Catalog> CreateCatalog(duckdb::AttachedDatabase &database,
                                                  std::shared_ptr<DataSource> data_source,
                                                  std::unique_ptr<CatalogSource> source) {
	return duckdb::make_uniq<AdapterCatalog>(database, std::move(data_source), std::move(source));
}

duckdb::unique_ptr<duckdb::TransactionManager> CreateTransactionManager(duckdb::AttachedDatabase &database) {
	return duckdb::make_uniq<AdapterTransactionManager>(database);
}

} // namespace scalardb_analytics_duckdb
