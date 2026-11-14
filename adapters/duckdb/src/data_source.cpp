#include "connector_api.hpp"
#include "scalardb_analytics_duckdb.hpp"

namespace scalardb_analytics_duckdb {

namespace {

// An error kind as the contract names it.
const char *KindName(ScalardbAnalyticsConnectorErrorKind kind) {
	switch (kind) {
	case SCALARDB_ANALYTICS_CONNECTOR_ERROR_KIND_INVALID_SETTINGS:
		return "invalid settings";
	case SCALARDB_ANALYTICS_CONNECTOR_ERROR_KIND_CONNECTION:
		return "connection";
	case SCALARDB_ANALYTICS_CONNECTOR_ERROR_KIND_AUTHENTICATION:
		return "authentication";
	case SCALARDB_ANALYTICS_CONNECTOR_ERROR_KIND_PERMISSION:
		return "permission";
	case SCALARDB_ANALYTICS_CONNECTOR_ERROR_KIND_INVALID_REQUEST:
		return "invalid request";
	case SCALARDB_ANALYTICS_CONNECTOR_ERROR_KIND_INVALID_STATE:
		return "invalid state";
	case SCALARDB_ANALYTICS_CONNECTOR_ERROR_KIND_STALE_REGISTRATION:
		return "stale registration";
	case SCALARDB_ANALYTICS_CONNECTOR_ERROR_KIND_DATA:
		return "data";
	case SCALARDB_ANALYTICS_CONNECTOR_ERROR_KIND_CANCELLATION:
		return "cancellation";
	case SCALARDB_ANALYTICS_CONNECTOR_ERROR_KIND_TIMEOUT:
		return "timeout";
	case SCALARDB_ANALYTICS_CONNECTOR_ERROR_KIND_RESOURCE:
		return "resource";
	case SCALARDB_ANALYTICS_CONNECTOR_ERROR_KIND_DATA_SOURCE:
		return "data source";
	case SCALARDB_ANALYTICS_CONNECTOR_ERROR_KIND_INTERNAL:
		return "internal";
	}
	throw duckdb::InternalException("ScalarDB Analytics returned an error of unknown kind %d", static_cast<int>(kind));
}

} // namespace

std::string Describe(const ScalardbAnalyticsConnectorError &error) {
	auto message = std::string(KindName(error.kind)) + ": " + Text(error.message);
	auto code = Text(error.data_source_code);
	return code.empty() ? message : message + " (Data Source code " + code + ")";
}

void Check(const ScalardbAnalyticsConnectorApi &api, ScalardbAnalyticsConnectorError *error, const char *action) {
	if (!error) {
		return;
	}
	auto kind = error->kind;
	auto message = Describe(*error);
	api.error_free(error);
	throw ConnectorException(kind, "ScalarDB Analytics could not " + std::string(action) + ": " + message);
}

DataSource::DataSource(const ScalardbAnalyticsConnectorApi &api_p, const std::string &connector,
                       const std::vector<std::pair<std::string, std::string>> &options,
                       uint64_t connection_timeout_milliseconds)
    : api(api_p), handle(nullptr) {
	std::vector<ScalardbAnalyticsConnectorOption> raw;
	for (auto &option : options) {
		raw.push_back({option.first.c_str(), option.second.c_str()});
	}
	Check(api,
	      api.data_source_open(connector.c_str(), raw.data(), raw.size(), connection_timeout_milliseconds, &handle),
	      "open the Data Source");
}

DataSource::~DataSource() {
	for (auto session : idle) {
		api.session_close(session);
	}
	api.data_source_close(handle);
}

DataSource::SessionLease DataSource::Session() {
	{
		std::lock_guard<std::mutex> guard(idle_lock);
		if (!idle.empty()) {
			auto session = idle.back();
			idle.pop_back();
			return SessionLease(*this, session);
		}
	}
	ScalardbAnalyticsConnectorSession *session = nullptr;
	Check(api, api.session_open(handle, &session), "open a session");
	return SessionLease(*this, session);
}

DataSource::SessionLease::SessionLease(DataSource &owner_p, ScalardbAnalyticsConnectorSession *session_p)
    : session(session_p), owner(&owner_p) {
}

DataSource::SessionLease::SessionLease(SessionLease &&other) noexcept : session(other.session), owner(other.owner) {
	other.session = nullptr;
}

void DataSource::SessionLease::Check(ScalardbAnalyticsConnectorError *error, const char *action) {
	if (error) {
		Discard();
		scalardb_analytics_duckdb::Check(owner->api, error, action);
	}
}

void DataSource::SessionLease::Discard() {
	if (session) {
		owner->api.session_close(session);
		session = nullptr;
	}
}

DataSource::SessionLease::~SessionLease() {
	if (!session) {
		return;
	}
	std::lock_guard<std::mutex> guard(owner->idle_lock);
	owner->idle.push_back(session);
}

TableMetadata::TableMetadata(const ScalardbAnalyticsConnectorTableMetadata &metadata_p,
                                       std::function<void()> release_p)
    : metadata(metadata_p), release(std::move(release_p)) {
}

TableMetadata::~TableMetadata() {
	release();
}

} // namespace scalardb_analytics_duckdb
