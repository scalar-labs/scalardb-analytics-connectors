#!/bin/sh
# Checks a distribution package as a user receives it: unpacked in a fresh
# directory, outside the development environment.
#
# 1. Every shipped binary loads only system libraries and the libraries in
#    the package's lib/ or its own directory, searches for them only relative
#    to itself, and is
#    named either relative to its search path or without a path; on
#    Linux, no glibc symbol is newer than the 2.28 baseline. A native library
#    that is neither is a dependency packaging/dependencies.json does not ship.
# 2. The extension carries DuckDB: it leaves no DuckDB symbol for the host
#    process to provide, and DuckDB's JDBC driver, which exports none of
#    DuckDB's C++ symbols, loads it (packaging/test/LoadExtension.java, with
#    the java on PATH).
# 3. The DuckDB CLI loads the packaged extension, which finds the packaged
#    library and ADBC driver by itself, and queries PostgreSQL
#    (adapters/duckdb/test/e2e.sh: in Docker, or the local server that
#    POSTGRESQL_TEST_PORT names).
# 4. In a package with bin/fetch-dependencies, for each dependency it lists:
#    its license is shown, fetching it without --accept-license changes
#    nothing in the package, and fetching it with --accept-license makes it
#    fetched.
# 5. Each connector's package test, crates/connectors/<connector>/tests/package/e2e.sh,
#    run with the unpacked package, its dependencies fetched, and the DuckDB
#    CLI. The test decides whether the package carries its connector and what
#    its Data Source needs, and prints its own TAP lines.
#
# Usage: check-package.sh <package .tgz> <DuckDB v1.5.6 CLI> <DuckDB JDBC 1.5.6.0 jar>
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
usage="usage: check-package.sh <package .tgz> <DuckDB CLI> <DuckDB JDBC jar>"
package=${1:?$usage}
duckdb=${2:?$usage}
jdbc=${3:?$usage}

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

# Every file but the header, the license texts, driver manifests, and the
# scripts in bin/.
binaries=$(find "$home" -type f ! -path "$home/include/*" ! -path "$home/LICENSES/*" ! -path "$home/bin/*" ! -name '*.toml')
# Whether a library of this file name ships in the package's lib/ or beside
# the binary that references it, as the libraries an ODBC driver needs do.
shipped() { [ -f "$home/lib/$1" ] || [ -f "$(dirname "$binary")/$1" ]; }
case "$(uname -s)" in
Darwin)
	for binary in $binaries; do
		# A library's own install name is listed among its references. A name
		# without a path, as an ADBC driver that is only loaded by its path may
		# carry, points nowhere outside the package.
		id=$(otool -D "$binary" | tail -n 1)
		case "$id" in
		@rpath/*) ;;
		*/*) echo "$binary is named $id" ;;
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
			libc.so.6 | libm.so.6 | libdl.so.2 | libpthread.so.0 | librt.so.1 | libresolv.so.2 | libgcc_s.so.1 | libstdc++.so.6 | ld-linux-*.so.*) ;;
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

extension=$home/duckdb/scalardb_analytics_connectors.duckdb_extension
# A weak reference (w) needs no definition: DuckDB's zstd leaves its tracing
# hooks weak and runs without them.
case "$(uname -s)" in
Darwin) undefined=$(nm -u "$extension" | grep duckdb || true) ;;
Linux) undefined=$(nm -D --undefined-only "$extension" | awk '$1 == "U"' | grep duckdb || true) ;;
esac
if [ -n "$undefined" ]; then
	fail "the extension leaves no DuckDB symbol to the host process"
	printf '%s\n' "$undefined" >&2
else
	echo "ok - the extension leaves no DuckDB symbol to the host process"
fi
if java -cp "$jdbc" "$root/packaging/test/LoadExtension.java" "$extension"; then
	echo "ok - DuckDB's JDBC driver loads the packaged extension"
else
	fail "DuckDB's JDBC driver loads the packaged extension"
fi

env -u ADBC_DRIVER_DIRECTORY -u SCALARDB_ANALYTICS_ADBC_DRIVER_DIRECTORY \
	sh "$root/adapters/duckdb/test/e2e.sh" "$duckdb" "$extension" ||
	fail "the packaged extension queries PostgreSQL with the packaged driver"

fetch=$home/bin/fetch-dependencies
if [ -f "$fetch" ]; then
	# The names are the first field of the lines that list one.
	names=$("$fetch" list | awk -F '\t' '$1 != "" { print $1 }')
	[ -n "$names" ] || fail "fetch-dependencies lists its dependencies"
	for name in $names; do
		if "$fetch" license "$name" | grep -q .; then
			echo "ok - fetch-dependencies shows the license of $name"
		else
			fail "fetch-dependencies shows the license of $name"
		fi
		find "$home" | sort >"$unpacked/before"
		if "$fetch" fetch "$name" >/dev/null 2>&1; then
			fail "fetch-dependencies fetches nothing of $name without --accept-license"
		elif find "$home" | sort | cmp -s - "$unpacked/before"; then
			echo "ok - fetch-dependencies fetches nothing of $name without --accept-license"
		else
			fail "fetch-dependencies fetches nothing of $name without --accept-license"
		fi
		if "$fetch" fetch "$name" --accept-license &&
			"$fetch" list | awk -F '\t' -v name="$name" '$1 == name && /: fetched$/ { found = 1 } END { exit !found }'; then
			echo "ok - fetch-dependencies fetches $name with --accept-license"
		else
			fail "fetch-dependencies fetches $name with --accept-license"
		fi
	done
fi

for test in "$root"/crates/connectors/*/tests/package/e2e.sh; do
	[ -f "$test" ] || continue
	connector=${test#"$root"/crates/connectors/}
	connector=${connector%%/*}
	sh "$test" "$home" "$duckdb" || fail "the package test of $connector passes"
done

[ "$failures" -eq 0 ] || { echo "$failures check(s) failed" >&2; exit 1; }
