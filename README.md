# ScalarDB Analytics Connector

This repository contains the language-neutral connector contract, the Rust
connector SDK, Connectors, the C API library that assembles
them, and the Query Engine adapters that call it. The shared Analytics type model and its Arrow schema conversion are
implemented. The shared connector planning and execution boundaries are also
implemented, and the PostgreSQL connector is implemented on them
([crates/connectors/postgresql](crates/connectors/postgresql/README.md)).
The C API library includes a Connector when it is built with that Connector's
feature, such as `cargo build -p connector-api --features postgresql`. The
DuckDB adapter attaches Data Sources to DuckDB through that library
([adapters/duckdb](adapters/duckdb/README.md)).

## Workspace layout

```text
crates/analytics-types/    Analytics types and Arrow type representation
crates/connector-sdk/      Data Source-independent connector processing
crates/connector-adbc/     ADBC access implementation
crates/connector-odbc/     ODBC access implementation
crates/connector-sql/      Shared SQL-family processing
crates/connectors/         Connector implementations
crates/connector-api/      C API implementation and connector assembly
adapters/duckdb/           DuckDB Query Engine adapter (C++, CMake), outside the Cargo workspace
packaging/                 Distribution package build and check
```

Dependencies point inward in this order:

```text
connector-api -> connector-postgresql -> connector-sql --> connector-sdk -> analytics-types
             |                       +-> connector-adbc -> connector-sdk
             +-> connector-mysql ------> connector-sql
             +-> connector-sdk -----------------------------------> analytics-types
connector-odbc -> connector-sdk
```

Connector crates do not depend on one another. Native dependencies belong in
ADBC driver artifacts, not in `connector-sdk` or a connector crate; the
exception is the ODBC driver manager that `connector-odbc` links. Adapters
call the C API that `connector-api` exports and depend on no crate.

Developer documentation starts at [docs/architecture.md](docs/architecture.md).
It explains the repository boundaries and links to the connector contract, the
shared Analytics type model, plan-pushdown model, type mappings, the connector SDK
design, and implementation conventions.

## Development

The Nix flake is the source of the development toolchain. Its default shell
provides the lock-file-pinned stable Rust toolchain, Clang, Protobuf tooling,
the DuckDB adapter's C++ build inputs and DuckDB CLI, unixODBC and the
PostgreSQL ODBC driver for `connector-odbc` and its tests, and repository
utilities.
Enter it directly or let direnv load it:

```sh
nix develop
# or
direnv allow
```

The workspace uses Rust 2024 and declares Rust 1.88 as its minimum supported
version, which the ODBC crates that `connector-odbc` uses require. The `msrv` shell
supplies exactly Rust 1.88.0. Run the same checks as CI through the flake. The
PostgreSQL connector's tests, the library's tests with its `postgresql`
feature, and `connector-odbc`'s tests also need Docker, which they use to start
a PostgreSQL server:

```sh
nix develop --command cargo fmt --all -- --check
nix develop --command cargo build --workspace --all-targets
nix develop --command cargo test --workspace --all-features
nix develop --command cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop --command cargo deny check bans licenses
nix develop .#msrv --command cargo build --workspace --all-targets
nix develop .#msrv --command cargo test --workspace
```

The DuckDB adapter builds against the C API library with the Connectors its
tests open, and its PostgreSQL tests also need Docker:

```sh
nix develop --command cargo build -p connector-api --features test-connector,postgresql
nix develop --command cmake -S adapters/duckdb -B target/duckdb-adapter -G Ninja
nix develop --command cmake --build target/duckdb-adapter
nix develop --command ctest --test-dir target/duckdb-adapter --output-on-failure
```

## Distribution package

Each release ships one self-contained package per platform, `osx_arm64`,
`linux_amd64`, and `linux_arm64`:
`scalardb-analytics-connectors-<version>-<platform>.tgz`. It works wherever it
is unpacked and needs only system libraries; the Linux packages need glibc 2.28
or later.

```text
lib/           The connector API library, with the connectors whose dependencies ship,
               and the native libraries it links that ship
duckdb/        The DuckDB v1.5.2 loadable extension, which finds ../lib
adbc-drivers/  The ADBC drivers that ship, with their manifests
include/       The C header of the connector API library
LICENSES/      The license texts of everything in the package
```

The extension uses the package's `adbc-drivers` unless
`SCALARDB_ANALYTICS_ADBC_DRIVER_DIRECTORY` names another directory
([Entry points](adapters/duckdb/README.md#entry-points)).

CI builds and checks the package of every platform for each pull request and
`main` push and keeps it as a workflow artifact
(`.github/workflows/package.yml`). Pushing a tag `v<version>` that equals the
workspace version publishes the packages as a GitHub Release. Which connectors
and dependencies ship is declared in `packaging/dependencies.json`
([Packaging](packaging/README.md)). The packages are built with
`packaging/package.sh`: on macOS in the Nix shell, and for Linux in
a manylinux_2_28 container that `packaging/linux/package.sh` starts with
Docker, for the Docker engine's architecture. Each is checked unpacked with
`packaging/check-package.sh` on its own platform:

```sh
# macOS arm64
nix develop --command packaging/package.sh
# Linux, on a host whose Docker engine runs Linux containers
packaging/linux/package.sh
packaging/check-package.sh target/dist/scalardb-analytics-connectors-<version>-<platform>.tgz <DuckDB v1.5.2 CLI>
```

### Versioning

The repository has one [semantic version](https://semver.org), held by
`[workspace.package]` in `Cargo.toml`. Every crate, the connector API library,
and the DuckDB adapter release under it, because the connectors are built into
one library and ship together. Before 1.0, a breaking change increases the
minor version.

## License and dependency policy

Repository-authored source code and documentation are licensed under Apache
License 2.0. Cargo packages declare `Apache-2.0` through workspace metadata.
Third-party and native dependencies must retain their own license notices and
be reviewed before distribution; native libraries must not be added to
`connector-sdk` or Connector crates, apart from the ODBC driver manager that
`connector-odbc` links. `deny.toml` states this policy for the
dependencies that ship in the library, and CI enforces it
([Dependency policy check](docs/connector-implementation.md#dependency-policy-check)).
