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
#   odbc-drivers/  the ODBC drivers, each in its own directory with the
#                  libraries it needs beyond the system, which the extension
#                  uses unless SCALARDB_ANALYTICS_ODBC_DRIVER_DIRECTORY is set
#   downloads/     a manifest for each dependency the package may not carry
#   bin/           fetch-dependencies (packaging/fetch-dependencies), which
#                  fetches those dependencies from their vendors as the
#                  manifests describe, each when the user names it and accepts
#                  its license
#   include/       the C header of the connector API library
#   LICENSES/      the license texts of everything shipped, and where to
#                  obtain the source code a license requires to be available
#
# A directory is present when a shipped connector uses it.
#
# The library is built with every connector feature of connector-api.
# packaging/dependencies.json declares the dependencies those connectors ship
# and those the DuckDB extension is built with; each dependency has a kind, and
# the ship_<kind> function below places it (packaging/README.md), or, for one
# delivered as `downloaded`, download writes its manifest, naming the
# directory its kind places it in. Nothing is
# built from a dependency's sources here: a dependency whose upstream
# distributes only sources is fetched as the artifact that
# scalar-labs/scalardb-analytics-third-party-deps built and published.
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
# connector crate, except those whose declaration lists `platforms` without
# this one.
features=$(printf '%s' "$metadata" | jq -r --slurpfile declaration "$declaration" --arg platform "$platform" '
	$declaration[0].connectors as $connectors
	| .packages[] | select(.name == "connector-api") | .features | to_entries
	| map(select(any(.value[]; startswith("dep:connector-"))) | .key)
	| map(select(($connectors[.].platforms // [$platform]) | index($platform)))
	| join(",")')

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

# A dependency whose upstream distributes only sources, built from them once
# and published as the release <dependency>-<version>[-r<revision>] of
# scalar-labs/scalardb-analytics-third-party-deps: its artifact is fetched into
# target/prebuilt, where a local build of that repository may also be put, and
# is used only if it matches its declared hash. Its installation prefix joins
# the prefixes the DuckDB extension's build searches, the libraries it lists
# under `lib` go to its `directory` (lib/ by default), and its LICENSES/ (each
# component's license texts and SOURCES.txt, where the sources are) to
# LICENSES/<dependency>/.
ship_prebuilt() {
	tag=$dependency-$(field '.version')
	if field '| has("revision")' >/dev/null; then
		tag=$tag-r$(field '.revision')
	fi
	artifact=$tag-$platform
	file=$root/target/prebuilt/$artifact.tar.gz
	hash=$(field '.artifacts[$platform] // ""')
	[ -n "$hash" ] || { echo "$dependency: no artifact hash is declared for $platform" >&2; exit 1; }
	if [ -f "$file" ]; then
		# A local build differs from the published artifact byte for byte.
		actual=sha256-$(openssl dgst -sha256 -binary "$file" | openssl base64 -A)
		[ "$actual" = "$hash" ] || {
			echo "$dependency: $file is not the declared artifact ($actual); remove it to download the declared one" >&2
			exit 1
		}
	else
		mkdir -p "$root/target/prebuilt"
		fetch "https://github.com/scalar-labs/scalardb-analytics-third-party-deps/releases/download/$tag/$artifact.tar.gz" "$hash" "$file"
	fi
	rm -rf "${work:?}/prebuilt/${dependency:?}"
	mkdir -p "$work/prebuilt/$dependency" "$stage/LICENSES/$dependency"
	tar -C "$work/prebuilt/$dependency" -xzf "$file"
	prefix=$work/prebuilt/$dependency/$artifact
	prefixes="${prefixes:+$prefixes;}$prefix"
	directory=$stage/$(field '.directory // "lib"')
	# The libraries the connector API library links, which its build finds
	# there.
	if [ "$directory" = "$stage/lib" ] && [ "$(field '.lib | length')" -gt 0 ]; then
		libraries="${libraries:+$libraries:}$prefix/lib"
	fi
	mkdir -p "$directory"
	for shipped in $(field '.lib[]'); do
		cp -L "$prefix/lib/$shipped" "$directory/"
	done
	cp -R "$prefix/LICENSES/." "$stage/LICENSES/$dependency/"
}

# Writes a TOML key and its string value, written as a JSON string, which is
# also a TOML basic string.
toml() { printf '%s = %s\n' "$1" "$(jq -n --arg value "$2" '$value')"; }

# A dependency the package may not carry, delivered as `downloaded`: described
# by downloads/<dependency>.toml, from which bin/fetch-dependencies fetches,
# when the user names the dependency and accepts its license, the first of the
# platform's downloads it can unpack where it runs, checks it against its
# declared hash, unpacks it by its format, and places each file the entry
# lists under `files` at its path below <place>, the package directory its
# kind places it in. Files the package carries there already stay.
download() {
	place=$1
	count=$(field '.downloads[$platform] | length')
	used_by=$(jq -r --arg dependency "$dependency" --arg features "$features" '
		[.connectors | to_entries[] | select(.key | IN($features | split(",")[]))
			| select(.value.dependencies | index($dependency)) | .key] | join(", ")' "$declaration")
	mkdir -p "$stage/downloads"
	{
		toml name "$(field '.name')"
		toml version "$(field '.version')"
		toml used_by "$used_by"
		toml license "$(field '.license')"
		toml directory "$place"
		printf '\n[files]\n'
		field '.files | to_entries[] | "\(.key | @json) = \(.value | @json)"'
		index=0
		while [ "$index" -lt "$count" ]; do
			format=$(field ".downloads[\$platform][$index].format")
			case $format in
			deb | rpm) ;;
			*) echo "$dependency: bin/fetch-dependencies cannot unpack the $format format" >&2; exit 1 ;;
			esac
			sha256=$(field ".downloads[\$platform][$index].hash" | sed 's/^sha256-//' |
				openssl base64 -d -A | od -An -v -tx1 | tr -d ' \n')
			printf '\n[[downloads]]\n'
			toml format "$format"
			toml url "$(field ".downloads[\$platform][$index].url")"
			toml sha256 "$sha256"
			index=$((index + 1))
		done
	} >"$stage/downloads/$dependency.toml"
}

# An ODBC driver, in odbc-drivers/<dependency>/.
download_odbc_driver() { download "odbc-drivers/$dependency"; }

# The dependencies of the shipping connectors and of the DuckDB extension,
# each once: shipped in the package, or, delivered as `downloaded`, described
# by a manifest for bin/fetch-dependencies.
prefixes=
libraries=
for dependency in $(jq -r --arg features "$features" '
	[(.connectors | to_entries[] | select(.key | IN($features | split(",")[])) | .value.dependencies[]),
		.extension.dependencies[]] | unique | .[]' "$declaration"); do
	kind=$(field '.kind' | tr - _)
	case $(field '.delivery // "bundled"') in
	bundled) step=ship_$kind ;;
	downloaded) step=download_$kind ;;
	*) echo "$dependency: unknown delivery $(field '.delivery')" >&2; exit 1 ;;
	esac
	command -v "$step" >/dev/null || { echo "$dependency: no $step for its kind" >&2; exit 1; }
	"$step"
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
if [ -d "$stage/downloads" ]; then
	mkdir -p "$stage/bin"
	cp "$root/packaging/fetch-dependencies" "$stage/bin/"
fi

# The connector API library, linked against the native libraries shipped
# above, such as the ODBC driver manager.
search=$libraries
if [ -n "${LIBRARY_PATH:-}" ]; then
	search="${search:+$search:}$LIBRARY_PATH"
fi
LIBRARY_PATH=$search \
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

# The DuckDB extension.
cmake -S "$root/adapters/duckdb" -B "$work/duckdb" \
	-DCMAKE_BUILD_TYPE=Release \
	-DSCALARDB_ANALYTICS_DISTRIBUTION=ON \
	-DSCALARDB_ANALYTICS_CONNECTOR_LIBRARY="$library" \
	-DCMAKE_PREFIX_PATH="$prefixes"
cmake --build "$work/duckdb" --parallel "$(getconf _NPROCESSORS_ONLN)" --target scalardb_analytics_connectors_extension
cp "$work/duckdb/scalardb_analytics_connectors.duckdb_extension" "$stage/duckdb/"

# License texts: this repository, DuckDB, which the extension carries, the
# Substrait types built into it, and the Rust crates in the library. The
# dependencies placed their own above.
cp "$root/LICENSE" "$stage/LICENSES/scalardb-analytics-connectors.txt"
for source in duckdb substrait; do
	cp "$work/duckdb/_deps/$source-src/LICENSE" "$stage/LICENSES/$source.txt"
done
cargo about generate --manifest-path "$root/crates/connector-api/Cargo.toml" \
	--features "$features" --config "$root/packaging/about.toml" \
	--output-file "$stage/LICENSES/rust-crates.txt" "$root/packaging/about.hbs"

tar -C "$work" -czf "$output/$name.tgz" "$name"
echo "$output/$name.tgz"
