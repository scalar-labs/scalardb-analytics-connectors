# Packaging

This directory builds and checks the
[distribution package](../README.md#distribution-package): one tgz per
platform that works wherever it is unpacked.

| File | Role |
| --- | --- |
| `dependencies.json` | The dependencies the connectors ship and the DuckDB extension is built with |
| `package.sh` | Builds the package of the running platform from that declaration |
| `linux/package.sh` | Runs `package.sh` for Linux in the pinned manylinux_2_28 image with Docker |
| `linux/build.sh` | Prepares the manylinux container and runs `package.sh` in it |
| `linux/check.sh` | Runs `test/run-check.sh` on a Linux package from a host that is not Linux, in the same image with Docker, beside a PostgreSQL container for the e2e |
| `linux/engine.sh` | Pins the manylinux_2_28 image for the Docker engine's architecture, for `linux/package.sh` and `linux/check.sh` |
| `test/run-check.sh` | Checks a package on its platform, as CI does: downloads the pinned DuckDB CLI and JDBC driver and runs `test/check-package.sh` |
| `test/check-package.sh` | Checks an unpacked package: binary references, the extension loaded through DuckDB's JDBC driver, and the DuckDB e2e |
| `test/LoadExtension.java` | Loads the packaged extension through DuckDB's JDBC driver, for `test/check-package.sh` |
| `about.toml`, `about.hbs` | cargo-about configuration and template for the Rust crates' license texts |

## What ships

The library is built with every connector feature of `connector-api`, the
features that enable a connector crate, and its Rust license texts cover them.
`sqlserver-odbc` is left out until the package ships the ODBC driver manager
that its library links.

`dependencies.json` declares what those connectors ship beyond the library,
and what the DuckDB extension is built with, in three parts:

- `connectors`: each key is a connector feature, which lists the entries it
  ships under `dependencies`. A connector that ships nothing beyond the
  library has no key.
- `extension`: the entries the DuckDB extension is built with, under
  `dependencies`, such as the static protobuf and abseil it links.
- `dependencies`: each entry has a `kind` and the fields that kind needs.
  An entry listed several times ships once. An entry no connector or the
  extension lists is not used yet.

A connector's README declares all its dependencies and whether each ships or
the user installs it
([Declaring dependencies](../docs/connector-implementation.md#declaring-dependencies)).
`dependencies.json` lists only the ones that ship, as far as their licenses
permit
([Shipping and installing dependencies](../docs/connector-implementation.md#shipping-and-installing-dependencies)).
A dependency the user installs is not listed here.

### Kinds

| Kind | Placed in | Fields |
| --- | --- | --- |
| `adbc-driver` | `adbc-drivers/<name>.toml` and the library it names, relative to it; license texts in `LICENSES/<name>/` | `name`, `version`, `library` and `licenses` (paths in the wheel), `wheels.<platform>.url` and `.hash` (SRI sha256) |
| `adbc-driver-archive` | As `adbc-driver`, from a per-platform tar.gz archive instead of a wheel, such as an ADBC Driver Foundry release | `name`, `version`, `licenses` (file paths in the archive), `archives.<platform>.url`, `.hash` (SRI sha256), and `.library` (path in the archive) |
| `prebuilt` | The libraries under `lib` in `lib/`; the installation prefix on the DuckDB extension build's `CMAKE_PREFIX_PATH`; the artifact's `LICENSES/` (license texts and `SOURCES.txt`) in `LICENSES/<name>/` ([Prebuilt dependencies](#prebuilt-dependencies)) | `version` and optional `revision` (naming the release), `lib` (file names in the artifact's `lib/`), and `artifacts.<platform>` (the artifact's SRI sha256) |

### Source code availability

An entry that contains a component whose license requires its source code to
be available with a binary, such as MPL-2.0, lists that component under
`sources`, each with `name`, `version`, `license`, and the `url` of its source
code at that version. `package.sh` writes the list to
`LICENSES/<name>/SOURCES.txt`. A `prebuilt` entry has no `sources`: its
artifact carries its own `SOURCES.txt`, naming the upstream sources it was
built from, which its release holds beside the artifacts
([Prebuilt dependencies](#prebuilt-dependencies)).

The versions are those built into the shipped binary, so they are read again
whenever the entry's version changes: for a driver built from Go, from the
module build information the binary embeds (`go version -m <library>`).

## Adding a connector

1. If the connector ships dependencies beyond the library, add its
   `connector-api` feature under `connectors`, with those dependencies. Its
   feature alone builds it into the library.
2. Declare each new dependency under `dependencies` with an existing kind, or
   add a kind as below.
3. Build and check the package on each platform. `test/check-package.sh`
   rejects a shipped binary that references a library which is neither a
   system library nor in the package's `lib/`: such a library is a dependency
   that is not declared, or is placed where nothing finds it.

## Adding a kind

A kind is a function `ship_<kind>` in `package.sh`, with `-` in the kind
written as `_`. It runs once per dependency of that kind, with `$dependency`
set to the entry's key; `field '<jq path>'` reads the entry and `fetch` downloads
a URL checked against its hash. It places the dependency in the staged package
(`$stage`) and copies its license texts into `$stage/LICENSES/`.

Where a kind places its files depends on what finds them:

- A native library the connector API library links goes to `lib/`. The library
  searches its own directory (`@loader_path`, `$ORIGIN`), so the library finds
  it there. Its own references to other non-system libraries must point to
  `lib/` as well, which the reference check verifies.
- A file the library loads by location, such as an ADBC driver, goes to the
  directory the library is told, as `adbc-drivers/` is told by the DuckDB
  extension. A new such location is a library setting, which changes the C ABI.

Add the kind to the table above.

## Prebuilt dependencies

A dependency is taken as its upstream's official prebuilt binaries when the
upstream publishes them, as the ADBC drivers are. A dependency whose upstream
distributes only sources, such as protobuf and abseil, unixODBC, or OpenSSL
and MIT Kerberos, is built from those unmodified sources once, by
[scalar-labs/scalardb-analytics-third-party-deps](https://github.com/scalar-labs/scalardb-analytics-third-party-deps),
which publishes the artifacts with the sources as GitHub Releases. No build
here builds such a dependency: `package.sh` fetches the artifact of the
release `<dependency>-<version>[-r<revision>]` and checks it against the hash
declared under `artifacts`, and the development shell (`flake.nix`) takes the
same artifact of protobuf. Third-party builds (distribution packages,
conda-forge) are not used.

To change such a dependency's version or build, release it in that
repository (its README, Releasing a dependency), then set the entry's
`version` and `revision` to the release and `artifacts` to the hashes its
publish step prints: the SHA-256 that the release's `<artifact>.tar.gz.sha256`
holds, in SRI form (`sha256-<base64>`).

`package.sh` takes an artifact from `target/prebuilt/` before downloading it,
still checked against its declared hash. A local build of that repository
differs from the published artifact byte for byte, so it is used only with its
own hash declared, to try a recipe change before it is released.

| Entry | Components | Platforms | Used by |
| --- | --- | --- | --- |
| `protobuf` | abseil, protobuf (static, with protoc) | osx_arm64, linux_amd64, linux_arm64 | The DuckDB extension, which links them statically, in the package and the development shell |
| `unixodbc` | unixODBC (libodbc, libodbcinst) | linux_amd64, linux_arm64 | Not yet: the ODBC driver manager of `sqlserver-odbc` |
| `kerberos` | OpenSSL 3, MIT Kerberos (without keyutils) | linux_amd64, linux_arm64 | Not yet: what Microsoft ODBC Driver 18 needs beyond the system |
