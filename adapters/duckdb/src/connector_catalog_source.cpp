#include "connector_api.hpp"
#include "scalardb_analytics_duckdb.hpp"

namespace scalardb_analytics_duckdb {
namespace {

// A name filter and sequence over its own copy of the names.
struct RawNames {
	explicit RawNames(Names names_p) : names(std::move(names_p)) {
		for (auto &name : names) {
			pointers.push_back(name.c_str());
			strings.push_back({name.c_str(), name.size()});
		}
	}

	ScalardbAnalyticsConnectorNameFilter Filter() const {
		return {pointers.data(), pointers.size()};
	}

	ScalardbAnalyticsConnectorNames Sequence() const {
		return {strings.data(), strings.size()};
	}

	Names names;
	std::vector<const char *> pointers;
	std::vector<ScalardbAnalyticsConnectorString> strings;
};

class Source : public CatalogSource {
public:
	Source(std::shared_ptr<DataSource> data_source_p, uint64_t timeout_p)
	    : data_source(std::move(data_source_p)), timeout(timeout_p) {
	}

	std::vector<Names> Namespaces() override {
		auto &api = data_source->api;
		auto lease = data_source->Session();
		ScalardbAnalyticsConnectorNamespaceList *list = nullptr;
		lease.Check(api.namespace_list(lease.session, nullptr, timeout, nullptr, &list), "list namespaces");
		std::vector<Names> result;
		for (size_t i = 0; i < list->entry_count; i++) {
			auto &entry = list->entries[i];
			if (entry.state == ScalardbAnalyticsConnectorListedState_ITEM) {
				result.push_back(NamePath(entry.namespace_));
			}
		}
		api.namespace_list_free(list);
		return result;
	}

	// Lists the namespaces of the path's own name, the last component: the
	// namespace is one listed with the whole path, and a range holding the
	// path that could not be read throws its reason.
	std::unique_ptr<NamespaceMetadata> Namespace(const Names &namespace_path) override {
		auto &api = data_source->api;
		auto lease = data_source->Session();
		RawNames filter({namespace_path.back()});
		auto namespaces = filter.Filter();
		ScalardbAnalyticsConnectorNamespaceList *list = nullptr;
		lease.Check(api.namespace_list(lease.session, &namespaces, timeout, nullptr, &list), "describe a namespace");
		std::unique_ptr<NamespaceMetadata> found;
		bool unreadable = false;
		std::string reason;
		for (size_t i = 0; i < list->entry_count; i++) {
			auto &entry = list->entries[i];
			if (entry.state == ScalardbAnalyticsConnectorListedState_ITEM) {
				if (NamePath(entry.namespace_) == namespace_path) {
					found = std::make_unique<NamespaceMetadata>(NamespaceMetadata {namespace_path});
				}
				continue;
			}
			auto scope = NamePath(entry.scope);
			if (scope.size() <= namespace_path.size() &&
			    std::equal(scope.begin(), scope.end(), namespace_path.begin())) {
				unreadable = true;
				reason = Text(entry.reason);
			}
		}
		api.namespace_list_free(list);
		if (!found && unreadable) {
			throw duckdb::IOException("ScalarDB Analytics could not read the namespace: %s", reason);
		}
		return found;
	}

	std::vector<Names> Tables(const Names &namespace_path) override {
		auto &api = data_source->api;
		auto lease = data_source->Session();
		// The namespace filter matches the namespace's own name, its last
		// component; the listed tables are then narrowed to the full path.
		RawNames filter({namespace_path.back()});
		auto namespaces = filter.Filter();
		ScalardbAnalyticsConnectorTableList *list = nullptr;
		lease.Check(api.table_list(lease.session, &namespaces, nullptr, timeout, nullptr, &list), "list tables");
		std::vector<Names> result;
		for (size_t i = 0; i < list->entry_count; i++) {
			auto &entry = list->entries[i];
			if (entry.state != ScalardbAnalyticsConnectorListedState_ITEM) {
				continue;
			}
			auto table = NamePath(entry.table);
			if (Names(table.begin(), table.end() - 1) == namespace_path) {
				result.push_back(std::move(table));
			}
		}
		api.table_list_free(list);
		return result;
	}

	std::unique_ptr<TableMetadata> Table(const Names &table) override {
		auto &api = data_source->api;
		auto lease = data_source->Session();
		RawNames names(table);
		auto sequence = names.Sequence();
		ScalardbAnalyticsConnectorTableMetadataResult *result = nullptr;
		lease.Check(api.table_metadata(lease.session, &sequence, 1, timeout, nullptr, &result), "read table metadata");
		auto release = [&api, result]() {
			api.table_metadata_result_free(result);
		};
		auto &record = result->records[0];
		switch (record.state) {
		case ScalardbAnalyticsConnectorTableRecordState_COMPLETE:
		case ScalardbAnalyticsConnectorTableRecordState_INCOMPLETE:
			return std::make_unique<TableMetadata>(*record.table_metadata, release);
		case ScalardbAnalyticsConnectorTableRecordState_DOES_NOT_EXIST:
			release();
			return nullptr;
		case ScalardbAnalyticsConnectorTableRecordState_PERMISSION_DENIED: {
			auto reason = Text(record.reason);
			release();
			throw duckdb::IOException("ScalarDB Analytics may not read the table metadata: %s", reason);
		}
		default: {
			// Not found: whether the table exists is unknown, so it is not
			// reported missing.
			auto reason = Text(record.reason);
			release();
			throw duckdb::IOException("ScalarDB Analytics could not find the table: %s", reason);
		}
		}
	}

private:
	std::shared_ptr<DataSource> data_source;
	// The limit of each request in milliseconds; 0 sets none.
	uint64_t timeout;
};

} // namespace

std::unique_ptr<CatalogSource> ConnectorCatalogSource(std::shared_ptr<DataSource> data_source,
                                                      uint64_t timeout_milliseconds) {
	return std::make_unique<Source>(std::move(data_source), timeout_milliseconds);
}

} // namespace scalardb_analytics_duckdb
