#!/bin/sh
# Checks a Linux distribution package on a host that is not Linux, such as
# macOS: packaging/test/run-check.sh, the check CI runs on its Linux runners,
# runs in a container of the manylinux_2_28 image that packaging/linux/engine.sh
# pins, with a JDK and the PostgreSQL client installed; bin/fetch-dependencies
# unpacks an rpm there with the image's rpm2archive. The servers for the DuckDB e2e run in containers whose
# network the check shares: PostgreSQL at 127.0.0.1:5432, and, for a package
# with the SQL Server through ODBC connector, which carries the manifest of
# its driver (downloads/msodbcsql18.toml), SQL Server at 127.0.0.1:1433 for
# that connector's package test, set up with its
# crates/connectors/sqlserver-odbc/tests/package/setup.sql. Only a package for
# the Docker engine's own architecture can be checked; the SQL Server image is
# linux/amd64 only, so on an arm64 engine it runs under the engine's
# emulation, while the check itself does not.
#
# Usage: packaging/linux/check.sh <package .tgz>
#
# POSTGRESQL_TEST_IMAGE selects the PostgreSQL server image (default
# postgres:18), and SQLSERVER_TEST_IMAGE the SQL Server one (default
# mcr.microsoft.com/mssql/server:2022-latest).
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
package=${1:?usage: packaging/linux/check.sh <package .tgz>}
[ -f "$package" ] || { echo "no package at $package" >&2; exit 1; }
# shellcheck source=packaging/linux/engine.sh
. "$root/packaging/linux/engine.sh"
case "$(basename "$package")" in
*-"$platform".tgz) ;;
*) echo "$package is not a $platform package, the Docker engine's platform" >&2; exit 1 ;;
esac
package=$(cd "$(dirname "$package")" && pwd)/$(basename "$package")

server=$(docker run -d -e POSTGRES_PASSWORD=connector-test-secret "${POSTGRESQL_TEST_IMAGE:-postgres:18}")
containers=$server
trap 'docker rm -f $containers >/dev/null' EXIT
if tar -tzf "$package" | grep -q '/downloads/msodbcsql18\.toml$'; then
	password=Connector-test-secret1
	sqlserver=$(docker run -d --platform linux/amd64 --network "container:$server" \
		-e ACCEPT_EULA=Y -e MSSQL_SA_PASSWORD="$password" \
		"${SQLSERVER_TEST_IMAGE:-mcr.microsoft.com/mssql/server:2022-latest}")
	containers="$containers $sqlserver"
	sqlcmd() {
		docker exec -i "$sqlserver" /opt/mssql-tools18/bin/sqlcmd -C -S 127.0.0.1 -U sa -P "$password" -b "$@"
	}
	attempts=0
	until sqlcmd -Q 'SELECT 1' >/dev/null 2>&1; do
		attempts=$((attempts + 1))
		[ "$attempts" -lt 240 ] || { echo "SQL Server did not become ready" >&2; exit 1; }
		sleep 0.5
	done
	sqlcmd <"$root/crates/connectors/sqlserver-odbc/tests/package/setup.sql" >/dev/null
	sqlserver_port=1433
fi
docker run --rm --network "container:$server" \
	-v "$root:/src:ro" -v "$package:/package.tgz:ro" -w /src \
	-e SQLSERVER_TEST_PORT="${sqlserver_port:-}" \
	"$image" sh -c 'dnf install -y -q java-21-openjdk-devel postgresql unzip openssl &&
		POSTGRESQL_TEST_PORT=5432 exec packaging/test/run-check.sh /package.tgz'
