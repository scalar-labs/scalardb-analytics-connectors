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
crates/connector-sql/      Shared SQL-family processing
crates/connectors/         Connector implementations
crates/connector-api/      C API implementation and connector assembly
adapters/duckdb/           DuckDB Query Engine adapter (C++, CMake), outside the Cargo workspace
```

Dependencies point inward in this order:

```text
connector-api -> connector-postgresql -> connector-sql -> connector-adbc -> connector-sdk -> analytics-types
             +-> connector-mysql ------> connector-sql
             +-> connector-sdk -------------------------------------------> analytics-types
```

Connector crates do not depend on one another. Native dependencies belong in
ADBC driver artifacts, not in `connector-sdk` or a connector crate. Adapters
call the C API that `connector-api` exports and depend on no crate.

Developer documentation starts at [docs/architecture.md](docs/architecture.md).
It explains the repository boundaries and links to the connector contract, the
shared Analytics type model, plan-pushdown model, type mappings, the connector SDK
design, and implementation conventions.

## Development

The Nix flake is the source of the development toolchain. Its default shell
provides the lock-file-pinned stable Rust toolchain, Clang, Protobuf tooling,
the DuckDB adapter's C++ build inputs and DuckDB CLI, and repository utilities.
Enter it directly or let direnv load it:

```sh
nix develop
# or
direnv allow
```

The workspace uses Rust 2024 and declares Rust 1.85 as its minimum supported
version. The `msrv` shell supplies exactly Rust 1.85.0. Run the same checks as
CI through the flake. The PostgreSQL connector's tests, and the library's tests
with its `postgresql` feature, also need Docker, which they use to start a
PostgreSQL server:

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

## License and dependency policy

Repository-authored source code and documentation are licensed under Apache
License 2.0. Cargo packages declare `Apache-2.0` through workspace metadata.
Third-party and native dependencies must retain their own license notices and
be reviewed before distribution; native libraries must not be added to
`connector-sdk` or Connector crates. `deny.toml` states this policy for the
dependencies that ship in the library, and CI enforces it
([Dependency policy check](docs/connector-implementation.md#dependency-policy-check)).
