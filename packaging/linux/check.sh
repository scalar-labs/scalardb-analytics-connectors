#!/bin/sh
# Checks a Linux distribution package on a host that is not Linux, such as
# macOS: packaging/test/run-check.sh, the check CI runs on its Linux runners,
# runs in a container of the manylinux_2_28 image that packaging/linux/engine.sh
# pins, with a JDK and the PostgreSQL client installed. A PostgreSQL server for
# the DuckDB e2e runs in a container whose network the check shares, where it
# is at 127.0.0.1:5432. Only a package for the Docker engine's own
# architecture can be checked.
#
# Usage: packaging/linux/check.sh <package .tgz>
#
# POSTGRESQL_TEST_IMAGE selects the server image (default postgres:18).
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
trap 'docker rm -f "$server" >/dev/null' EXIT
docker run --rm --network "container:$server" \
	-v "$root:/src:ro" -v "$package:/package.tgz:ro" -w /src \
	"$image" sh -c 'dnf install -y -q java-21-openjdk-devel postgresql unzip openssl &&
		POSTGRESQL_TEST_PORT=5432 exec packaging/test/run-check.sh /package.tgz'
