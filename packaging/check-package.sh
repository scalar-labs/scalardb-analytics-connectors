#!/bin/sh
# Checks a distribution package as a user receives it: unpacked in a fresh
# directory, outside the development environment.
#
# 1. Every shipped binary loads only system libraries and the libraries in
#    the package's lib/, and searches for them only relative to itself; on
#    Linux, no glibc symbol is newer than the 2.28 baseline. A native library
#    that is neither is a dependency packaging/dependencies.json does not ship.
# 2. The DuckDB CLI loads the packaged extension, which finds the packaged
#    library and ADBC driver by itself, and queries PostgreSQL
#    (adapters/duckdb/test/e2e.sh: in Docker, or the local server that
#    POSTGRESQL_TEST_PORT names).
#
# Usage: check-package.sh <package .tgz> <DuckDB v1.5.6 CLI>
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
package=${1:?usage: check-package.sh <package .tgz> <DuckDB CLI>}
duckdb=${2:?usage: check-package.sh <package .tgz> <DuckDB CLI>}

unpacked=$(mktemp -d)
trap 'rm -rf "$unpacked"' EXIT
tar -C "$unpacked" -xzf "$package"
set -- "$unpacked"/*
[ $# -eq 1 ] || { echo "the package must hold exactly one top-level directory" >&2; exit 1; }
home=$1
failures=0

fail() {
	echo "not ok - $1" >&2
	failures=$((failures + 1))
}

# Every file but the header, the license texts, and driver manifests.
binaries=$(find "$home" -type f ! -path "$home/include/*" ! -path "$home/LICENSES/*" ! -name '*.toml')
# Whether a library of this file name ships in the package's lib/.
shipped() { [ -f "$home/lib/$1" ]; }
case "$(uname -s)" in
Darwin)
	for binary in $binaries; do
		# A library's own install name is listed among its references.
		id=$(otool -D "$binary" | tail -n 1)
		case "$id" in
		@rpath/*) ;;
		*) echo "$binary is named $id" ;;
		esac
		otool -L "$binary" | tail -n +2 | awk '{ print $1 }' | while read -r reference; do
			case "$reference" in
			/usr/lib/* | /System/Library/*) ;;
			"$id") ;;
			@rpath/*) shipped "${reference#@rpath/}" || echo "$binary references $reference" ;;
			*) echo "$binary references $reference" ;;
			esac
		done
		otool -l "$binary" | awk '/cmd LC_RPATH/ { getline; getline; print $2 }' | while read -r rpath; do
			case "$rpath" in
			@loader_path | @loader_path/*) ;;
			*) echo "$binary searches $rpath" ;;
			esac
		done
	done >"$unpacked/references"
	;;
Linux)
	for binary in $binaries; do
		readelf -d "$binary" | awk '/\(NEEDED\)/ { gsub(/[][]/, "", $5); print $5 }' | while read -r needed; do
			case "$needed" in
			libc.so.6 | libm.so.6 | libdl.so.2 | libpthread.so.0 | librt.so.1 | libgcc_s.so.1 | libstdc++.so.6 | ld-linux-*.so.*) ;;
			*) shipped "$needed" || echo "$binary needs $needed" ;;
			esac
		done
		readelf -d "$binary" | awk '/\((RPATH|RUNPATH)\)/ { gsub(/[][]/, "", $5); print $5 }' | tr ':' '\n' | while read -r rpath; do
			case "$rpath" in
			'$ORIGIN' | '$ORIGIN'/*) ;;
			*) echo "$binary searches $rpath" ;;
			esac
		done
		objdump -T "$binary" | grep -o 'GLIBC_[0-9.]*' | sort -u -V | while read -r symbol; do
			newest=$(printf '%s\nGLIBC_2.28\n' "$symbol" | sort -V | tail -n 1)
			[ "$newest" = GLIBC_2.28 ] || echo "$binary uses $symbol"
		done
	done >"$unpacked/references"
	;;
esac
if [ -s "$unpacked/references" ]; then
	fail "shipped binaries reference only the system and the package"
	cat "$unpacked/references" >&2
else
	echo "ok - shipped binaries reference only the system and the package"
fi

for file in lib/libconnector_api.* duckdb/scalardb_analytics_connectors.duckdb_extension include/scalardb_analytics_connector.h \
	LICENSES/scalardb-analytics-connectors.txt LICENSES/rust-crates.txt; do
	set -- "$home"/$file
	[ -f "$1" ] || fail "the package holds $file"
done

env -u ADBC_DRIVER_DIRECTORY -u SCALARDB_ANALYTICS_ADBC_DRIVER_DIRECTORY \
	sh "$root/adapters/duckdb/test/e2e.sh" "$duckdb" "$home/duckdb/scalardb_analytics_connectors.duckdb_extension" ||
	fail "the packaged extension queries PostgreSQL with the packaged driver"

[ "$failures" -eq 0 ] || { echo "$failures check(s) failed" >&2; exit 1; }
