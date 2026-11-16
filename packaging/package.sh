#!/bin/sh
# Builds the distribution package of the running platform:
#
#   target/dist/scalardb-analytics-connectors-<version>-<platform>.tgz
#
# The package is self-contained and works wherever it is unpacked:
#
#   lib/           the connector API library, with the shipping connectors,
#                  and any native library it links that ships
#   duckdb/        the DuckDB loadable extension, which finds ../lib
#   adbc-drivers/  the ADBC drivers and their manifests, which the extension
#                  uses unless SCALARDB_ANALYTICS_ADBC_DRIVER_DIRECTORY is set
#   include/       the C header of the connector API library
#   LICENSES/      the license texts of everything shipped, and where to
#                  obtain the source code a license requires to be available
#
# The library is built with every connector feature of connector-api.
# packaging/dependencies.json declares the dependencies those connectors ship;
# each dependency has a kind, and the ship_<kind> function below places it
# (packaging/README.md).
#
# Usage: package.sh
#
# Requires cargo, cargo-about, cmake, make, a C++17 compiler, curl, unzip,
# and jq; the Nix development shell provides them on macOS, and
# packaging/linux/package.sh runs this script in a manylinux container.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
output=$root/target/dist
mkdir -p "$output"
declaration=$root/packaging/dependencies.json

case "$(uname -s)-$(uname -m)" in
Darwin-arm64) platform=osx_arm64 suffix=dylib ;;
Linux-x86_64) platform=linux_amd64 suffix=so ;;
Linux-aarch64) platform=linux_arm64 suffix=so ;;
*) echo "no distribution package is defined for $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

metadata=$(cargo metadata --no-deps --format-version 1 --manifest-path "$root/Cargo.toml")
version=$(printf '%s' "$metadata" | jq -r '.packages[] | select(.name == "connector-api") | .version')
target=$(printf '%s' "$metadata" | jq -r '.target_directory')
name=scalardb-analytics-connectors-$version-$platform
work=$target/package/$platform
stage=$work/$name
rm -rf "$stage"
mkdir -p "$stage/lib" "$stage/duckdb" "$stage/include" "$stage/LICENSES"

# The shipping connectors are the connector-api features that enable a
# connector crate. sqlserver-odbc is left out until the package ships the ODBC
# driver manager that its library links.
features=$(printf '%s' "$metadata" | jq -r '
	.packages[] | select(.name == "connector-api") | .features | to_entries
	| map(select(any(.value[]; startswith("dep:connector-"))) | .key)
	| map(select(. != "sqlserver-odbc")) | join(",")')

# Reads a field of the dependency being shipped: field '<jq path below it>'.
field() {
	jq -er --arg dependency "$dependency" --arg platform "$platform" \
		".dependencies[\$dependency]$1" "$declaration"
}

# Downloads a URL to a file and checks it against an SRI sha256 hash.
fetch() {
	curl -fsSL -o "$3" "$1"
	actual=sha256-$(openssl dgst -sha256 -binary "$3" | openssl base64 -A)
	[ "$actual" = "$2" ] || { echo "$dependency: $1 does not match its hash: $actual" >&2; exit 1; }
}

# Places an ADBC driver library, given its path, in adbc-drivers/ with a
# manifest that names it relative to the manifest.
place_adbc_driver() {
	file=$(basename "$1")
	mkdir -p "$stage/adbc-drivers" "$stage/LICENSES/$dependency"
	cp "$1" "$stage/adbc-drivers/$file"
	cat >"$stage/adbc-drivers/$dependency.toml" <<EOF
manifest_version = 1
name = "$(field '.name')"
version = "$(field '.version')"

[Driver]
shared = "$file"
EOF
}

# An ADBC driver from its pinned Python wheel: the library goes to
# adbc-drivers/, and the wheel's license texts go to LICENSES/<dependency>/.
ship_adbc_driver() {
	wheel=$work/$dependency.whl
	fetch "$(field '.wheels[$platform].url')" "$(field '.wheels[$platform].hash')" "$wheel"
	rm -rf "${work:?}/${dependency:?}"
	unzip -q -d "$work/$dependency" "$wheel"
	place_adbc_driver "$work/$dependency/$(field '.library')"
	cp "$work/$dependency/$(field '.licenses')"/* "$stage/LICENSES/$dependency/"
}

# An ADBC driver from its pinned per-platform tar.gz archive, such as an ADBC
# Driver Foundry release: the library goes to adbc-drivers/, and the license
# files the archive holds go to LICENSES/<dependency>/.
ship_adbc_driver_archive() {
	archive=$work/$dependency.tar.gz
	fetch "$(field '.archives[$platform].url')" "$(field '.archives[$platform].hash')" "$archive"
	rm -rf "${work:?}/${dependency:?}"
	mkdir -p "$work/$dependency"
	tar -C "$work/$dependency" -xzf "$archive"
	place_adbc_driver "$work/$dependency/$(field '.archives[$platform].library')"
	for license in $(field '.licenses[]'); do
		cp "$work/$dependency/$license" "$stage/LICENSES/$dependency/"
	done
}

# The connector API library.
cargo build --release --manifest-path "$root/Cargo.toml" -p connector-api --features "$features"
library=$target/release/libconnector_api.$suffix
cp "$library" "$stage/lib/"
if [ "$platform" = osx_arm64 ]; then
	# The Nix toolchain links its own libiconv; the system's has the same ABI.
	for iconv in $(otool -L "$stage/lib/libconnector_api.dylib" | awk '/\/nix\/store\/.*libiconv/ { print $1 }'); do
		install_name_tool -change "$iconv" /usr/lib/libiconv.2.dylib "$stage/lib/libconnector_api.dylib"
	done
	codesign --force --sign - "$stage/lib/libconnector_api.dylib"
fi
cp "$root/crates/connector-api/include/scalardb_analytics_connector.h" "$stage/include/"

# The dependencies of the shipping connectors, each once.
for dependency in $(jq -r '[.connectors[].dependencies[]] | unique | .[]' "$declaration"); do
	kind=$(field '.kind' | tr - _)
	command -v "ship_$kind" >/dev/null || { echo "$dependency: no ship_$kind for its kind" >&2; exit 1; }
	"ship_$kind"
	# Where to obtain the source code of the components the dependency
	# contains under a license that requires it to be available with a binary.
	if field '| has("sources")' >/dev/null; then
		{
			printf 'The source code of these components of %s %s,\n' "$(field '.name')" "$(field '.version')"
			printf 'whose licenses require it to be available, is at:\n\n'
			field '.sources[] | "- \(.name) \(.version) (\(.license)): \(.url)"'
		} >"$stage/LICENSES/$dependency/SOURCES.txt"
	fi
done

# The DuckDB extension.
cmake -S "$root/adapters/duckdb" -B "$work/duckdb" \
	-DCMAKE_BUILD_TYPE=Release \
	-DSCALARDB_ANALYTICS_DISTRIBUTION=ON \
	-DSCALARDB_ANALYTICS_CONNECTOR_LIBRARY="$library"
cmake --build "$work/duckdb" --parallel "$(getconf _NPROCESSORS_ONLN)" --target scalardb_analytics_connectors_extension
cp "$work/duckdb/scalardb_analytics_connectors.duckdb_extension" "$stage/duckdb/"

# License texts: this repository, the C++ libraries and the Substrait types
# built into the extension, DuckDB, which the extension carries, and the Rust
# crates in the library.
cp "$root/LICENSE" "$stage/LICENSES/scalardb-analytics-connectors.txt"
for source in protobuf absl duckdb substrait; do
	cp "$work/duckdb/_deps/$source-src/LICENSE" "$stage/LICENSES/$source.txt"
done
cp "$work/duckdb/_deps/protobuf-src/third_party/utf8_range/LICENSE" "$stage/LICENSES/utf8_range.txt"
cargo about generate --manifest-path "$root/crates/connector-api/Cargo.toml" \
	--features "$features" --config "$root/packaging/about.toml" \
	--output-file "$stage/LICENSES/rust-crates.txt" "$root/packaging/about.hbs"

tar -C "$work" -czf "$output/$name.tgz" "$name"
echo "$output/$name.tgz"
