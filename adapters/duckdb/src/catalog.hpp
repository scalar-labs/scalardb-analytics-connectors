// The DuckDB catalog of one attached Data Source.
//
// Schemas are the catalog source's namespaces and tables its tables. Both are
// read when DuckDB first looks them up and kept for the life of the attached
// catalog. Every write is rejected.
#pragma once

#include <atomic>
#include <map>
#include <memory>
#include <mutex>
#include <set>

#include "duckdb/catalog/catalog.hpp"
#include "duckdb/catalog/catalog_entry/schema_catalog_entry.hpp"
#include "duckdb/catalog/catalog_entry/table_catalog_entry.hpp"
#include "scalardb_analytics_duckdb.hpp"
#include "types.hpp"

namespace scalardb_analytics_duckdb {

// What a scan needs to know about a table: its binding and base schema.
struct TableShape {
	// The table's name path from its metadata, which plans read it by.
	Names binding;
	// The columns with an Analytics type, in ordinal order; DuckDB's column
	// indexes are positions in this list.
	std::vector<Field> fields;
	// The columns without an Analytics type, in ordinal order, each with why
	// it has none. A query may name one but not read it.
	std::vector<std::pair<std::string, std::string>> unmapped;
	// Set when the Connector reported the shape stale; the table's next
	// lookup describes the table again.
	mutable std::atomic<bool> stale {false};
};

// The virtual columns of a table and its scans: DuckDB's empty column, and
// the columns without an Analytics type, which a query can name so that
// reading one fails with the reason instead of as an unknown column. They are
// not part of `*`. The table offers no `rowid`, which a Data Source's rows do
// not have.
duckdb::virtual_column_map_t VirtualColumns(const TableShape &shape);

class AdapterCatalog;

class AdapterTable : public duckdb::TableCatalogEntry {
public:
	AdapterTable(duckdb::Catalog &catalog, duckdb::SchemaCatalogEntry &schema, duckdb::CreateTableInfo &info,
	             std::unique_ptr<TableMetadata> metadata, std::shared_ptr<const TableShape> shape);

	duckdb::unique_ptr<duckdb::BaseStatistics> GetStatistics(duckdb::ClientContext &context,
	                                                         duckdb::column_t column_id) override;
	duckdb::TableFunction GetScanFunction(duckdb::ClientContext &context,
	                                      duckdb::unique_ptr<duckdb::FunctionData> &bind_data) override;
	duckdb::TableStorageInfo GetStorageInfo(duckdb::ClientContext &context) override;
	duckdb::virtual_column_map_t GetVirtualColumns() const override;
	// A Data Source's rows have no row identifier; a write then reaches the
	// read-only catalog instead of failing while DuckDB binds the row ids.
	duckdb::vector<duckdb::column_t> GetRowIdColumns() const override;

	std::unique_ptr<TableMetadata> metadata;
	std::shared_ptr<const TableShape> shape;
};

class AdapterSchema : public duckdb::SchemaCatalogEntry {
public:
	AdapterSchema(AdapterCatalog &catalog, duckdb::CreateSchemaInfo &info, Names namespace_path);

	void Scan(duckdb::ClientContext &context, duckdb::CatalogType type,
	          const std::function<void(duckdb::CatalogEntry &)> &callback) override;
	void Scan(duckdb::CatalogType type, const std::function<void(duckdb::CatalogEntry &)> &callback) override;
	duckdb::optional_ptr<duckdb::CatalogEntry> LookupEntry(duckdb::CatalogTransaction transaction,
	                                                       const duckdb::EntryLookupInfo &lookup_info) override;

	duckdb::optional_ptr<duckdb::CatalogEntry> CreateIndex(duckdb::CatalogTransaction transaction,
	                                                       duckdb::CreateIndexInfo &info,
	                                                       duckdb::TableCatalogEntry &table) override;
	duckdb::optional_ptr<duckdb::CatalogEntry> CreateFunction(duckdb::CatalogTransaction transaction,
	                                                          duckdb::CreateFunctionInfo &info) override;
	duckdb::optional_ptr<duckdb::CatalogEntry> CreateTable(duckdb::CatalogTransaction transaction,
	                                                       duckdb::BoundCreateTableInfo &info) override;
	duckdb::optional_ptr<duckdb::CatalogEntry> CreateView(duckdb::CatalogTransaction transaction,
	                                                      duckdb::CreateViewInfo &info) override;
	duckdb::optional_ptr<duckdb::CatalogEntry> CreateSequence(duckdb::CatalogTransaction transaction,
	                                                          duckdb::CreateSequenceInfo &info) override;
	duckdb::optional_ptr<duckdb::CatalogEntry> CreateTableFunction(duckdb::CatalogTransaction transaction,
	                                                               duckdb::CreateTableFunctionInfo &info) override;
	duckdb::optional_ptr<duckdb::CatalogEntry> CreateCopyFunction(duckdb::CatalogTransaction transaction,
	                                                              duckdb::CreateCopyFunctionInfo &info) override;
	duckdb::optional_ptr<duckdb::CatalogEntry> CreatePragmaFunction(duckdb::CatalogTransaction transaction,
	                                                                duckdb::CreatePragmaFunctionInfo &info) override;
	duckdb::optional_ptr<duckdb::CatalogEntry> CreateCollation(duckdb::CatalogTransaction transaction,
	                                                           duckdb::CreateCollationInfo &info) override;
	duckdb::optional_ptr<duckdb::CatalogEntry> CreateType(duckdb::CatalogTransaction transaction,
	                                                      duckdb::CreateTypeInfo &info) override;
	void DropEntry(duckdb::ClientContext &context, duckdb::DropInfo &info) override;
	void Alter(duckdb::CatalogTransaction transaction, duckdb::AlterInfo &info) override;

private:
	// Reads the table list once, then each table on its first lookup; a name
	// not listed is described by its own request.
	duckdb::optional_ptr<duckdb::CatalogEntry> Table(const std::string &name);
	void LoadTableNames();

	Names namespace_path;
	std::mutex lock;
	bool tables_listed = false;
	// Table name to its name path, for every listed or described table.
	std::map<std::string, Names> table_paths;
	std::map<std::string, duckdb::unique_ptr<AdapterTable>> tables;
	// Names whose description said they do not exist, kept like the other
	// entries for the attached catalog's life.
	std::set<std::string> missing;
	// Entries replaced after their shape went stale, kept because a bound
	// query may still use them.
	std::vector<duckdb::unique_ptr<AdapterTable>> retired;
};

class AdapterCatalog : public duckdb::Catalog {
public:
	AdapterCatalog(duckdb::AttachedDatabase &database, std::shared_ptr<DataSource> data_source,
	               std::unique_ptr<CatalogSource> source);

	void Initialize(bool load_builtin) override;
	std::string GetCatalogType() override;
	duckdb::optional_ptr<duckdb::CatalogEntry> CreateSchema(duckdb::CatalogTransaction transaction,
	                                                        duckdb::CreateSchemaInfo &info) override;
	duckdb::optional_ptr<duckdb::SchemaCatalogEntry> LookupSchema(duckdb::CatalogTransaction transaction,
	                                                              const duckdb::EntryLookupInfo &schema_lookup,
	                                                              duckdb::OnEntryNotFound if_not_found) override;
	void ScanSchemas(duckdb::ClientContext &context,
	                 std::function<void(duckdb::SchemaCatalogEntry &)> callback) override;
	duckdb::PhysicalOperator &PlanCreateTableAs(duckdb::ClientContext &context, duckdb::PhysicalPlanGenerator &planner,
	                                            duckdb::LogicalCreateTable &op, duckdb::PhysicalOperator &plan) override;
	duckdb::PhysicalOperator &PlanInsert(duckdb::ClientContext &context, duckdb::PhysicalPlanGenerator &planner,
	                                     duckdb::LogicalInsert &op,
	                                     duckdb::optional_ptr<duckdb::PhysicalOperator> plan) override;
	duckdb::PhysicalOperator &PlanDelete(duckdb::ClientContext &context, duckdb::PhysicalPlanGenerator &planner,
	                                     duckdb::LogicalDelete &op, duckdb::PhysicalOperator &plan) override;
	duckdb::PhysicalOperator &PlanUpdate(duckdb::ClientContext &context, duckdb::PhysicalPlanGenerator &planner,
	                                     duckdb::LogicalUpdate &op, duckdb::PhysicalOperator &plan) override;
	duckdb::DatabaseSize GetDatabaseSize(duckdb::ClientContext &context) override;
	bool InMemory() override;
	std::string GetDBPath() override;
	void DropSchema(duckdb::ClientContext &context, duckdb::DropInfo &info) override;

	std::shared_ptr<DataSource> data_source;
	std::unique_ptr<CatalogSource> source;

private:
	void LoadSchemas();

	std::mutex lock;
	bool schemas_loaded = false;
	std::map<std::string, duckdb::unique_ptr<AdapterSchema>> schemas;
	// Schema names that stand for no namespace or whose description said it
	// does not exist, kept for the attached catalog's life.
	std::set<std::string> missing_schemas;
};

} // namespace scalardb_analytics_duckdb
