#!/bin/sh
# End-to-end test of the loadable extension: the DuckDB CLI loads the built
# extension, which initializes the connector API library itself, and queries a
# PostgreSQL server in Docker through it, as a user would.
#
# Usage: e2e.sh <duckdb CLI> <extension file>
#
# The server runs in the container the PostgreSQL connector's own tests use;
# POSTGRESQL_TEST_IMAGE selects its image (default postgres:18). Where Docker
# is unavailable, as on macOS CI runners, POSTGRESQL_TEST_PORT names a running
# local server instead, whose `postgres` user has the test password and which
# the local psql reaches. The ADBC driver comes from ADBC_DRIVER_DIRECTORY,
# which the Nix development shell sets, or, when it is unset, from the
# distribution package the extension is in.
set -eu

duckdb=$1
extension=$2
password=connector-test-secret
failures=0

case "$("$duckdb" --version)" in
v1.5.2*) ;;
*) echo "the DuckDB CLI is not v1.5.2: $("$duckdb" --version)" >&2; exit 1 ;;
esac

if [ -n "${POSTGRESQL_TEST_PORT:-}" ]; then
	port=$POSTGRESQL_TEST_PORT
	psql() {
		PGPASSWORD=$password command psql -h 127.0.0.1 -p "$port" -U postgres -v ON_ERROR_STOP=1 -qAt "$@"
	}
else
	image=${POSTGRESQL_TEST_IMAGE:-postgres:18}
	container=scalardb-analytics-connector-test-$(printf '%s' "$image" | tr ':/.' '---')
	# The server, started if needed.
	if [ "$(docker inspect -f '{{.State.Running}}' "$container" 2>/dev/null || true)" != true ]; then
		docker start "$container" >/dev/null 2>&1 ||
			docker run -d --name "$container" -e POSTGRES_PASSWORD="$password" -p 127.0.0.1::5432 "$image" >/dev/null
	fi
	port=$(docker port "$container" 5432/tcp | head -n 1 | sed 's/.*://')
	psql() {
		docker exec -i "$container" psql -h 127.0.0.1 -U postgres -v ON_ERROR_STOP=1 -qAt "$@"
	}
fi
attempts=0
until psql -c 'SELECT 1' >/dev/null 2>&1; do
	attempts=$((attempts + 1))
	[ "$attempts" -lt 240 ] || { echo "PostgreSQL did not become ready" >&2; exit 1; }
	sleep 0.25
done
psql <<'SQL'
DROP SCHEMA IF EXISTS duckdb_adapter_e2e CASCADE;
CREATE SCHEMA duckdb_adapter_e2e;
CREATE TABLE duckdb_adapter_e2e.orders (
    id bigint PRIMARY KEY, customer text, amount numeric(10,2), price double precision);
INSERT INTO duckdb_adapter_e2e.orders VALUES
    (1, 'alice', 10.50, 1.25), (2, 'bob', 20.00, 2.5), (3, 'carol', NULL, 3.75);
SQL

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# Without ADBC_DRIVER_DIRECTORY, as when testing an unpacked distribution
# package, the extension uses the package's own drivers.
if [ -n "${ADBC_DRIVER_DIRECTORY:-}" ]; then
	export SCALARDB_ANALYTICS_ADBC_DRIVER_DIRECTORY="$ADBC_DRIVER_DIRECTORY"
else
	unset SCALARDB_ANALYTICS_ADBC_DRIVER_DIRECTORY
fi
attach="ATTACH 'postgresql' AS pg (TYPE scalardb_analytics, host '127.0.0.1', port $port,
    database 'postgres', user 'postgres', password '$password', sslmode 'disable');"

# Runs SQL in a fresh CLI process after loading the extension; prints its CSV
# rows without a header, or, when it prints none, its error output. The
# extension loads in a batch of its own: DuckDB resolves a PRAGMA while parsing
# its batch, before a LOAD in the same batch has run.
run() {
	out=$("$duckdb" -unsigned -csv -noheader -cmd "LOAD '$extension'" -c "$1" 2>"$work/stderr" || true)
	if [ -n "$out" ]; then printf '%s' "$out"; else cat "$work/stderr"; fi
}

# Compares a case's output with the expected text.
expect() {
	if [ "$2" = "$3" ]; then
		echo "ok - $1"
	else
		echo "not ok - $1"
		printf '  expected:\n%s\n  actual:\n%s\n' "$2" "$3"
		failures=$((failures + 1))
	fi
}

# Checks that a case's output contains, or does not contain, a text.
contains() {
	if printf '%s' "$3" | grep -qF -- "$2"; then found=yes; else found=no; fi
	if [ "$found" = "$4" ]; then
		echo "ok - $1"
	else
		echo "not ok - $1: expected '$2' to be $([ "$4" = yes ] && echo present || echo absent)"
		printf '%s\n' "$3"
		failures=$((failures + 1))
	fi
}

orders=pg.duckdb_adapter_e2e.orders

expect "complete pushdown returns the filtered and projected rows" "2,bob
3,carol" "$(run "$attach SELECT id, customer FROM $orders WHERE id >= 2 ORDER BY id;")"

plan=$(run "$attach EXPLAIN SELECT id, customer FROM $orders WHERE id >= 2;")
contains "complete pushdown leaves no local plan" "Local Plan: no" "$plan" yes
contains "complete pushdown leaves no filter in DuckDB" "FILTER" "$plan" no

expect "partial pushdown returns the rows of the local filter" "2
3" "$(run "$attach SELECT id FROM $orders WHERE price > 2.0 ORDER BY id;")"
contains "partial pushdown runs the filter locally" "Local Filter: yes" \
	"$(run "$attach EXPLAIN SELECT id FROM $orders WHERE price > 2.0;")" yes

# The filter removes a row with an amount, so both aggregates change if it is
# lost; the remaining NULL amount is skipped by sum.
expect "an aggregate over a pushed filter" "2,20.00" \
	"$(run "$attach SELECT count(*), sum(amount) FROM $orders WHERE id >= 2;")"

expect "a join with a DuckDB table" "alice,gold
bob,silver" "$(run "$attach CREATE TEMP TABLE tiers AS SELECT * FROM (VALUES (1, 'gold'), (2, 'silver')) t(id, tier);
    SELECT o.customer, t.tier FROM $orders o JOIN tiers t USING (id) ORDER BY o.id;")"

contains "writes are rejected" "read-only" \
	"$(run "$attach INSERT INTO $orders (id) VALUES (4);")" yes

cat >"$work/sources.json" <<JSON
[{"name": "pg", "connector": "postgresql", "options": {"host": "127.0.0.1", "port": $port,
  "database": "postgres", "user": "postgres", "password": "$password", "sslmode": "disable"}}]
JSON
# Enabling logging makes the CLI print a notice, so the result row is tagged.
bulk=$(run "CALL enable_logging(); PRAGMA scalardb_analytics_attach('$work/sources.json');
    SELECT 'result=' || (SELECT count(*) FROM $orders) || ','
        || (SELECT count(*) FROM duckdb_logs() WHERE message LIKE '%$password%');")
expect "the bulk-attach pragma attaches the Data Source, and its password stays out of the log" "result=3,0" \
	"$(printf '%s\n' "$bulk" | grep -o 'result=[0-9]*,[0-9]*' || printf '%s' "$bulk")"

[ "$failures" -eq 0 ] || { echo "$failures case(s) failed" >&2; exit 1; }
