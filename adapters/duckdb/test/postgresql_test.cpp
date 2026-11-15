// Pushdown through the PostgreSQL Connector, against a PostgreSQL server in
// Docker: the container the connector's own tests use. The ADBC driver comes
// from ADBC_DRIVER_DIRECTORY, which the Nix development shell sets, and
// POSTGRESQL_TEST_IMAGE selects the server image (default `postgres:18`).
//
// The PostgreSQL Connector moves a filter to the source only when PostgreSQL
// compares its values as the Analytics types do; a filter on a floating-point
// column stays local over a remote read of the whole table.
#include <array>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <thread>

#include "catch2/catch_test_macros.hpp"
#include "catch2/matchers/catch_matchers_string.hpp"
#include "support.hpp"

using Catch::Matchers::ContainsSubstring;
using test_support::Database;

namespace {

const char *const PASSWORD = "connector-test-secret";

// Runs a shell command and returns its exit status and standard output.
std::pair<int, std::string> Run(const std::string &command) {
	std::string output;
	auto pipe = popen((command + " 2>/dev/null").c_str(), "r");
	REQUIRE(pipe);
	std::array<char, 256> buffer;
	while (fgets(buffer.data(), buffer.size(), pipe)) {
		output += buffer.data();
	}
	return {pclose(pipe), output};
}

// A PostgreSQL server in Docker, started once for the test process.
struct Server {
	Server() {
		auto image = std::getenv("POSTGRESQL_TEST_IMAGE") ? std::getenv("POSTGRESQL_TEST_IMAGE") : "postgres:18";
		container = "scalardb-analytics-connector-test-" + std::string(image);
		for (auto &c : container) {
			if (c == ':' || c == '/' || c == '.') {
				c = '-';
			}
		}
		auto running = Run("docker inspect -f '{{.State.Running}}' " + container);
		if (running.first != 0) {
			// Another test process may create the same container concurrently.
			if (Run("docker run -d --name " + container + " -e POSTGRES_PASSWORD=" + PASSWORD +
			        " -p 127.0.0.1::5432 " + image)
			        .first != 0) {
				REQUIRE(Run("docker start " + container).first == 0);
			}
		} else if (running.second.find("true") == std::string::npos) {
			REQUIRE(Run("docker start " + container).first == 0);
		}
		auto published = Run("docker port " + container + " 5432/tcp").second;
		port = published.substr(published.rfind(':') + 1);
		port.erase(port.find_last_not_of(" \n") + 1);
		auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(60);
		while (Psql("SELECT 1").first != 0) {
			REQUIRE(std::chrono::steady_clock::now() < deadline);
			std::this_thread::sleep_for(std::chrono::milliseconds(250));
		}
	}

	std::pair<int, std::string> Psql(const std::string &sql) {
		return Run("docker exec " + container +
		           " psql -h 127.0.0.1 -U postgres -v ON_ERROR_STOP=1 -qAt -c \"" + sql + "\"");
	}

	std::string container;
	std::string port;
};

Server &PostgreSQL() {
	static Server server;
	static bool loaded = [] {
		auto result = server.Psql(
		    "DROP SCHEMA IF EXISTS duckdb_adapter CASCADE; CREATE SCHEMA duckdb_adapter; "
		    "CREATE TABLE duckdb_adapter.items (id bigint PRIMARY KEY, name text, price double precision); "
		    "INSERT INTO duckdb_adapter.items VALUES (1, 'one', 1.25), (2, 'two', 2.5), (3, NULL, 3.75); "
		    "CREATE TYPE duckdb_adapter.mood AS ENUM ('sad', 'happy'); "
		    "CREATE TABLE duckdb_adapter.typed (flag boolean, small smallint, number integer, ratio real, "
		    "amount numeric(10,2), raw bytea, day date, at time, local timestamp, created timestamptz, "
		    "span interval, reference uuid, doc jsonb, mood duckdb_adapter.mood, zoned timetz); "
		    "INSERT INTO duckdb_adapter.typed VALUES (true, 7, 42, 0.5, 10.50, '\\x0102', '2026-01-02', "
		    "'10:20:30.123456', '2026-01-02 10:20:30', '2026-01-01 00:00:00+00', '1 mon 2 days 3 seconds', "
		    "'6f1e1c2a-0000-4000-8000-000000000001', '{\\\"a\\\": 1}', 'happy', '10:00:00+09'), "
		    "(NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL), "
		    "(NULL, NULL, 43, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, "
		    "'6f1e1c2a-0000-4000-8000-000000000001', NULL, 'sad', NULL), "
		    "(NULL, NULL, 44, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, "
		    "'6f1e1c2a-0000-4000-8000-000000000002', NULL, 'happy', NULL); "
		    "CREATE TYPE duckdb_adapter.spaced AS ENUM (E'tab\\\\there'); "
		    "CREATE TABLE duckdb_adapter.labels (label duckdb_adapter.spaced); "
		    "INSERT INTO duckdb_adapter.labels VALUES (E'tab\\\\there'); "
		    "CREATE TABLE duckdb_adapter.scaled (n numeric(5,-2), id integer); "
		    "INSERT INTO duckdb_adapter.scaled VALUES (12345, 1), (-700, 2), (NULL, 3); "
		    "CREATE TABLE duckdb_adapter.unmapped (tags text[], area numeric); "
		    "INSERT INTO duckdb_adapter.unmapped VALUES ('{a}', 1);");
		REQUIRE(result.first == 0);
		return true;
	}();
	(void)loaded;
	return server;
}

void AttachPostgreSQL(Database &database) {
	auto &server = PostgreSQL();
	database.Rows("ATTACH 'postgresql' AS pg (TYPE scalardb_analytics_connectors, host '127.0.0.1', port " + server.port +
	              ", database 'postgres', user 'postgres', password '" + PASSWORD + "', sslmode 'disable')");
}

const char *const ITEMS = "pg.duckdb_adapter.items";

} // namespace

TEST_CASE("complete pushdown runs only the remote plan") {
	Database database;
	AttachPostgreSQL(database);
	auto query = std::string("SELECT id, name FROM ") + ITEMS + " WHERE id >= 2";
	auto plan = database.Explain(query);
	CHECK(plan.find("FILTER") == std::string::npos);
	CHECK_THAT(plan, ContainsSubstring("Local Plan: no"));
	CHECK(database.Rows(query + " ORDER BY id") ==
	      std::vector<std::vector<std::string>> {{"2", "two"}, {"3", "NULL"}});
}

TEST_CASE("partial pushdown runs the local plan over the remote output") {
	Database database;
	AttachPostgreSQL(database);
	// The floating-point filter stays local; the remote read returns every
	// column, including ones only the local filter uses.
	auto query = std::string("SELECT id FROM ") + ITEMS + " WHERE price > 2.0";
	auto plan = database.Explain(query);
	CHECK(plan.find("FILTER") == std::string::npos);
	CHECK_THAT(plan, ContainsSubstring("Local Plan: yes"));
	CHECK_THAT(plan, ContainsSubstring("Local Filter: yes"));
	CHECK(database.Rows(query + " ORDER BY id") == std::vector<std::vector<std::string>> {{"2"}, {"3"}});
}

TEST_CASE("a filter pushed to PostgreSQL and a filter kept local apply together") {
	Database database;
	AttachPostgreSQL(database);
	// `id >= 2` runs in PostgreSQL and the floating-point `price < 3.0` in the
	// local plan; each removes a row the other keeps.
	auto query = std::string("SELECT id FROM ") + ITEMS + " WHERE id >= 2 AND price < 3.0";
	auto plan = database.Explain(query);
	CHECK(plan.find("FILTER") == std::string::npos);
	CHECK_THAT(plan, ContainsSubstring("Local Filter: yes"));
	CHECK(database.Rows(query) == std::vector<std::vector<std::string>> {{"2"}});
}

TEST_CASE("a table without a column of an Analytics type cannot be read") {
	Database database;
	AttachPostgreSQL(database);
	auto error = database.Error("SELECT count(*) FROM pg.duckdb_adapter.unmapped");
	CHECK_THAT(error, ContainsSubstring("none of its columns has an Analytics type"));
	CHECK_THAT(error, ContainsSubstring("tags (text[]"));
	CHECK_THAT(error, ContainsSubstring("area (numeric"));
}

TEST_CASE("a negative-scale numeric is read as its scaled value") {
	Database database;
	AttachPostgreSQL(database);
	// numeric(5,-2) rounds to hundreds; the adapter reads the unscaled
	// integers and multiplies them back.
	CHECK(database.Rows("SELECT typeof(n), n, id FROM pg.duckdb_adapter.scaled ORDER BY id") ==
	      std::vector<std::vector<std::string>> {{"DOUBLE", "12300.0", "1"}, {"DOUBLE", "-700.0", "2"}, {"DOUBLE", "NULL", "3"}});
}

TEST_CASE("an Enum whose label needs a JSON escape is read") {
	Database database;
	AttachPostgreSQL(database);
	// The adapter and the Connector spell the tab differently in the Enum's
	// JSON parameters; the planned and returned types are still the same.
	CHECK(database.Rows("SELECT label::VARCHAR = 'tab' || chr(9) || 'here' FROM pg.duckdb_adapter.labels") ==
	      std::vector<std::vector<std::string>> {{"true"}});
}

TEST_CASE("PostgreSQL values arrive as the mapped DuckDB types") {
	Database database;
	AttachPostgreSQL(database);
	CHECK(database.Rows("SELECT column_name, data_type FROM information_schema.columns WHERE table_catalog = 'pg' "
	                    "AND table_name = 'typed' ORDER BY ordinal_position") ==
	      std::vector<std::vector<std::string>> {{"flag", "BOOLEAN"},
	                                             {"small", "SMALLINT"},
	                                             {"number", "INTEGER"},
	                                             {"ratio", "FLOAT"},
	                                             {"amount", "DECIMAL(10,2)"},
	                                             {"raw", "BLOB"},
	                                             {"day", "DATE"},
	                                             {"at", "TIME"},
	                                             {"local", "TIMESTAMP"},
	                                             {"created", "TIMESTAMP WITH TIME ZONE"},
	                                             {"span", "INTERVAL"},
	                                             {"reference", "UUID"},
	                                             {"doc", "JSON"},
	                                             {"mood", "ENUM('sad', 'happy')"},
	                                             {"zoned", "VARCHAR"}});
	database.Rows("SET TimeZone = 'UTC'");
	auto typed = "pg.duckdb_adapter.typed";
	CHECK(database.Rows(std::string("SELECT * FROM ") + typed + " WHERE flag") ==
	      std::vector<std::vector<std::string>> {{"true", "7", "42", "0.5", "10.50", "\\x01\\x02", "2026-01-02",
	                                              "10:20:30.123456", "2026-01-02 10:20:30", "2026-01-01 00:00:00+00",
	                                              "1 month 2 days 00:00:03", "6f1e1c2a-0000-4000-8000-000000000001",
	                                              "{\"a\": 1}", "happy", "10:00:00+09"}});
	// One condition per query, each over rows it tells apart.
	CHECK(database.Rows(std::string("SELECT count(*) FROM ") + typed + " WHERE flag IS NULL") ==
	      std::vector<std::vector<std::string>> {{"3"}});
	CHECK(database.Rows(std::string("SELECT count(*) FROM ") + typed + " WHERE mood IS NULL") ==
	      std::vector<std::vector<std::string>> {{"1"}});
	// Equality on Uuid and Enum is offered and compares their values.
	CHECK(database.Rows(std::string("SELECT number FROM ") + typed +
	                    " WHERE reference = '6f1e1c2a-0000-4000-8000-000000000001' ORDER BY number") ==
	      std::vector<std::vector<std::string>> {{"42"}, {"43"}});
	auto happy = std::string("SELECT number FROM ") + typed + " WHERE mood = 'happy' ORDER BY number";
	CHECK(database.Explain(happy).find("FILTER") == std::string::npos);
	CHECK(database.Rows(happy) == std::vector<std::vector<std::string>> {{"42"}, {"44"}});
}
