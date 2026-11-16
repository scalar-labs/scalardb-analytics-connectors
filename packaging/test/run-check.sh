#!/bin/sh
# Checks a distribution package on the platform it is for: downloads the
# DuckDB CLI and JDBC driver of the pinned versions, each checked against its
# hash, and runs check-package.sh with them. CI runs it on each platform's
# runner, and packaging/linux/check.sh runs it in a Linux container.
#
# Usage: run-check.sh <package .tgz>
#
# Requires curl, unzip, openssl, and a JDK's java on PATH. The DuckDB e2e
# starts a PostgreSQL server with Docker, or uses the one POSTGRESQL_TEST_PORT
# names (adapters/duckdb/test/e2e.sh).
set -eu

package=${1:?usage: run-check.sh <package .tgz>}
root=$(cd "$(dirname "$0")/../.." && pwd)
duckdb_url=https://github.com/duckdb/duckdb/releases/download/v1.5.6
# DuckDB's JDBC driver, which loads the extension without DuckDB's C++
# symbols in the host process.
jdbc_url=https://repo1.maven.org/maven2/org/duckdb/duckdb_jdbc/1.5.6.0/duckdb_jdbc-1.5.6.0.jar
jdbc_sha256=04ae3375d7c40dc88dc9bf0014b82ba5e66715564584af4e3abea39c7ba144d3
case "$(uname -s)-$(uname -m)" in
Darwin-arm64) cli=duckdb_cli-osx-universal.zip cli_sha256=80a80c68736bd7dea53e8b02447e9759db0c8596ea156476ba396c44f54c5810 ;;
Linux-x86_64) cli=duckdb_cli-linux-amd64.zip cli_sha256=6e89deac1ebbc36eed0291caf8b567b030c7b86ac35998f71854e22b3c5d5e2f ;;
Linux-aarch64) cli=duckdb_cli-linux-arm64.zip cli_sha256=c544e92c9b7c31fc53c2139802cabd8e2d1b2b3e3f933117f31611239c1402db ;;
*) echo "no DuckDB CLI is pinned for $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

tools=$(mktemp -d)
trap 'rm -rf "$tools"' EXIT
# Downloads a URL to a file and checks it against a sha256 hash.
fetch() {
	curl -fsSL -o "$2" "$1"
	actual=$(openssl dgst -sha256 -r "$2" | cut -d ' ' -f 1)
	[ "$actual" = "$3" ] || { echo "$1 does not match its hash: $actual" >&2; exit 1; }
}
fetch "$duckdb_url/$cli" "$tools/cli.zip" "$cli_sha256"
unzip -q -d "$tools" "$tools/cli.zip"
fetch "$jdbc_url" "$tools/duckdb_jdbc.jar" "$jdbc_sha256"

"$root/packaging/test/check-package.sh" "$package" "$tools/duckdb" "$tools/duckdb_jdbc.jar"
