#!/bin/sh
# The SQL Server through ODBC connector in an unpacked distribution package:
# the DuckDB CLI loads the package's extension and queries SQL Server through
# the connector, as a user would, after the driver was fetched with
# bin/fetch-dependencies. The driver must use the ODBC driver manager,
# Kerberos, and OpenSSL that the package carries, wherever the system has its
# own. packaging/test/check-package.sh runs it with every connector's package
# test; it prints one TAP line per case.
#
# Usage: e2e.sh <unpacked package> <DuckDB CLI>
#
# A package without the connector, which carries no manifest of its driver
# (downloads/msodbcsql18.toml), has nothing to check. The server runs in the
# container this crate's own tests use, whose image is linux/amd64 only:
# SQLSERVER_TEST_IMAGE selects it (default
# mcr.microsoft.com/mssql/server:2022-latest). SQLSERVER_TEST_PORT instead
# names a running server at 127.0.0.1, which the caller has set up with
# setup.sql and whose `sa` login has the test password, as
# packaging/linux/check.sh does. Without it, an arm64 host skips the queries,
# since it cannot run the server's image.
set -eu

home=$1
duckdb=$2
extension=$home/duckdb/scalardb_analytics_connectors.duckdb_extension
password=Connector-test-secret1
failures=0
here=$(cd "$(dirname "$0")" && pwd)

[ -f "$home/downloads/msodbcsql18.toml" ] || exit 0
if [ -z "${SQLSERVER_TEST_PORT:-}" ] && [ "$(uname -m)" != x86_64 ]; then
	echo "ok - the packaged extension reads SQL Server # SKIP no SQL Server: its image is linux/amd64 only"
	exit 0
fi

if [ -n "${SQLSERVER_TEST_PORT:-}" ]; then
	port=$SQLSERVER_TEST_PORT
else
	image=${SQLSERVER_TEST_IMAGE:-mcr.microsoft.com/mssql/server:2022-latest}
	container=scalardb-analytics-connector-test-$(printf '%s' "$image" | tr ':/.' '---')
	# The server, started if needed.
	if [ "$(docker inspect -f '{{.State.Running}}' "$container" 2>/dev/null || true)" != true ]; then
		docker start "$container" >/dev/null 2>&1 ||
			docker run -d --platform linux/amd64 --name "$container" -e ACCEPT_EULA=Y \
				-e MSSQL_SA_PASSWORD="$password" -p 127.0.0.1::1433 "$image" >/dev/null
	fi
	port=$(docker port "$container" 1433/tcp | head -n 1 | sed 's/.*://')
	sqlcmd() {
		docker exec -i "$container" /opt/mssql-tools18/bin/sqlcmd -C -S 127.0.0.1 -U sa -P "$password" -b "$@"
	}
	attempts=0
	until sqlcmd -Q 'SELECT 1' >/dev/null 2>&1; do
		attempts=$((attempts + 1))
		[ "$attempts" -lt 240 ] || { echo "SQL Server did not become ready" >&2; exit 1; }
		sleep 0.5
	done
	sqlcmd <"$here/setup.sql" >/dev/null
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# The extension uses the package's own ODBC drivers.
unset SCALARDB_ANALYTICS_ODBC_DRIVER_DIRECTORY

# Runs SQL in a fresh CLI process after loading the extension, with the
# environment assignments given first; prints its CSV rows without a header,
# or, when it prints none, its error output.
run() {
	sql=$1
	shift
	out=$(env "$@" "$duckdb" -unsigned -csv -noheader -cmd "LOAD '$extension'" -c "$sql" 2>"$work/stderr" || true)
	if [ -n "$out" ]; then printf '%s' "$out"; else cat "$work/stderr"; fi
}

expect() {
	if [ "$2" = "$3" ]; then
		echo "ok - $1"
	else
		echo "not ok - $1"
		printf '  expected:\n%s\n  actual:\n%s\n' "$2" "$3"
		failures=$((failures + 1))
	fi
}

attach() {
	echo "ATTACH 'sqlserver-odbc' AS ss (TYPE scalardb_analytics_connectors,
    server '127.0.0.1,$port', database 'package_e2e', user 'sa', password '$password', $1);"
}
query="SELECT id, customer FROM ss.dbo.orders WHERE id >= 2 ORDER BY id;"

expect "an encrypted connection reads SQL Server" "2,bob
3,carol" "$(run "$(attach "encrypt 'yes', trust_server_certificate 'yes'") $query")"
expect "an unencrypted connection reads SQL Server" "2,bob
3,carol" "$(run "$(attach "encrypt 'no'") $query")"

# The dynamic loader names each library it initializes; those the driver
# needs beyond the C runtime must be the package's.
LD_DEBUG=libs LD_DEBUG_OUTPUT=$work/ld run "$(attach "encrypt 'yes', trust_server_certificate 'yes'") $query" >/dev/null
cat "$work"/ld.* | sed -n 's/.*calling init: //p' | sort -u >"$work/initialized"
outside=$(grep -E '/lib(odbc|odbcinst|ssl|crypto|krb5|gssapi_krb5|k5crypto|krb5support|com_err)\.so' "$work/initialized" |
	grep -v "^$home/" || true)
used=$(grep -cE "^$home/odbc-drivers/msodbcsql18/lib/lib(ssl|krb5)\.so" "$work/initialized" || true)
if [ -z "$outside" ] && [ "$used" -eq 2 ]; then
	echo "ok - the driver uses the package's driver manager, Kerberos, and OpenSSL"
else
	echo "not ok - the driver uses the package's driver manager, Kerberos, and OpenSSL"
	cat "$work/initialized"
	failures=$((failures + 1))
fi

[ "$failures" -eq 0 ] || { echo "$failures case(s) failed" >&2; exit 1; }
