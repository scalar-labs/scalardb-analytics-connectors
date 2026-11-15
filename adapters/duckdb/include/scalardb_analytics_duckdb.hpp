// The DuckDB adapter of ScalarDB Analytics.
//
// Each Data Source attaches to DuckDB as one read-only catalog. Its schemas
// and tables come from a catalog source, and its scans plan their filters and
// projections through the connector API's pushdown planning.
//
// The loadable extension owns the connector API library. An application that
// embeds DuckDB and owns an initialized library uses this header instead.
#pragma once

#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <utility>
#include <vector>

#include "duckdb.hpp"
#include "scalardb_analytics_connector.h"

namespace scalardb_analytics_duckdb {

using Names = std::vector<std::string>;

// An open Data Source and the sessions the catalog and its scans use.
//
// A session serves one call at a time, so the Data Source lends each caller
// its own session and keeps returned ones for reuse. Sessions are closed
// before the Data Source.
class DataSource {
public:
	// A session lent to one caller; it returns to the Data Source when dropped.
	class SessionLease {
	public:
		SessionLease(SessionLease &&other) noexcept;
		SessionLease(const SessionLease &) = delete;
		SessionLease &operator=(const SessionLease &) = delete;
		~SessionLease();

		// Throws a failed call's error after closing the session, which the
		// failure may have left unusable; does nothing for a null error.
		void Check(ScalardbAnalyticsConnectorError *error, const char *action);
		// Closes the session instead of returning it.
		void Discard();

		ScalardbAnalyticsConnectorSession *session;

	private:
		friend class DataSource;
		SessionLease(DataSource &owner, ScalardbAnalyticsConnectorSession *session);
		DataSource *owner;
	};

	// Opens a Data Source of the named Connector with its connection options.
	// The connection timeout limits connecting while the Data Source and its
	// sessions open; 0 sets no limit.
	DataSource(const ScalardbAnalyticsConnectorApi &api, const std::string &connector,
	           const std::vector<std::pair<std::string, std::string>> &options,
	           uint64_t connection_timeout_milliseconds);
	DataSource(const DataSource &) = delete;
	DataSource &operator=(const DataSource &) = delete;
	~DataSource();

	// Lends an idle session, opening one when none is idle.
	SessionLease Session();

	const ScalardbAnalyticsConnectorApi &api;

private:
	ScalardbAnalyticsConnectorDataSource *handle;
	std::mutex idle_lock;
	std::vector<ScalardbAnalyticsConnectorSession *> idle;
};

// One table's metadata as the connector API structures, owned by the adapter.
//
// The catalog source decides how the structures are released; the adapter
// decides when, which is when the table's catalog entry is dropped.
class TableMetadata {
public:
	TableMetadata(const ScalardbAnalyticsConnectorTableMetadata &metadata, std::function<void()> release);
	TableMetadata(const TableMetadata &) = delete;
	TableMetadata &operator=(const TableMetadata &) = delete;
	~TableMetadata();

	const ScalardbAnalyticsConnectorTableMetadata &metadata;

private:
	std::function<void()> release;
};

// One namespace's metadata: what the connector API reports of a namespace,
// which is its name path.
struct NamespaceMetadata {
	Names names;
};

// Where a catalog's namespaces, tables, and their metadata come from.
//
// The adapter reads them from the Connector itself
// (`ConnectorCatalogSource`). A component that keeps registered metadata,
// such as the Physical Catalog, supplies its own source instead.
//
// A list holds the names the source could read and serves enumeration. A
// name that is not listed is described before it is reported missing: only
// its own request tells that it does not exist from that it could not be
// read, and a source that cannot read it throws the reason.
class CatalogSource {
public:
	virtual ~CatalogSource() = default;

	// Lists the namespaces, each as its name path.
	virtual std::vector<Names> Namespaces() = 0;
	// Reads one namespace's metadata, or returns null when the namespace does
	// not exist.
	virtual std::unique_ptr<NamespaceMetadata> Namespace(const Names &namespace_path) = 0;
	// Lists the tables of one namespace, each as its name path.
	virtual std::vector<Names> Tables(const Names &namespace_path) = 0;
	// Reads one table's metadata, or returns null when the table does not exist.
	//
	// The table's name path in the metadata is the binding that plans read the
	// table by.
	virtual std::unique_ptr<TableMetadata> Table(const Names &table) = 0;
};

// Reads the catalog from the Connector of an open Data Source; the timeout
// limits each request, and 0 sets no limit.
std::unique_ptr<CatalogSource> ConnectorCatalogSource(std::shared_ptr<DataSource> data_source,
                                                      uint64_t timeout_milliseconds);

// Creates the catalog of one attached Data Source from a catalog source.
duckdb::unique_ptr<duckdb::Catalog> CreateCatalog(duckdb::AttachedDatabase &database,
                                                  std::shared_ptr<DataSource> data_source,
                                                  std::unique_ptr<CatalogSource> source);

// Creates the transaction manager of a catalog made by `CreateCatalog`.
duckdb::unique_ptr<duckdb::TransactionManager> CreateTransactionManager(duckdb::AttachedDatabase &database);

// Registers the `scalardb_analytics_connectors` attach type and the bulk-attach pragma.
//
// `api` belongs to an initialized library that outlives the database.
void Register(duckdb::DatabaseInstance &database, const ScalardbAnalyticsConnectorApi &api);

} // namespace scalardb_analytics_duckdb
