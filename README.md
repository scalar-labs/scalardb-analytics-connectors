# ScalarDB Analytics Connector

This repository contains the language-neutral connector contract, the Rust
connector SDK, source-specific connectors, and the C API library that assembles
them. The shared Analytics type model and its Arrow schema conversion are
implemented. The shared connector planning and execution boundaries are also
implemented; source-specific connector behavior remains out of scope.

## Workspace layout

```text
contract/                  Machine-readable, language-neutral contract
crates/analytics-types/    Analytics types and Arrow type representation
crates/connector-sdk/      Source-independent connector processing
crates/connector-adbc/     ADBC access implementation
crates/connector-sql/      Shared SQL-family processing
crates/connectors/         Source-specific connectors
crates/connector-api/      C API implementation and connector assembly
scripts/                   Repository validation entry points
```

Dependencies point inward in this order:

```text
connector-api -> connector-postgresql -> connector-sql -> connector-adbc -> connector-sdk -> analytics-types
             +-> connector-mysql ------> connector-sql
             +-> connector-sdk -------------------------------------------> analytics-types
```

Connector crates do not depend on one another. Native dependencies belong in
ADBC driver artifacts, not in `connector-sdk` or a connector crate.

Developer documentation starts at [docs/architecture.md](docs/architecture.md).
It explains the repository boundaries and links to the canonical mental models
for the shared Analytics type system and plan pushdown.

## Development

The Nix flake is the source of the development toolchain. Its default shell
provides the lock-file-pinned stable Rust toolchain, Clang, Python, and
repository utilities. Enter it directly or let direnv load it:

```sh
nix develop
# or
direnv allow
```

The workspace uses Rust 2024 and declares Rust 1.85 as its minimum supported
version. The `msrv` shell supplies exactly Rust 1.85.0. Run the same checks as
CI through the flake:

```sh
nix develop --command python3 scripts/validate_contracts.py
nix develop --command cargo fmt --all -- --check
nix develop --command cargo build --workspace --all-targets
nix develop --command cargo test --workspace
nix develop --command cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop .#msrv --command cargo build --workspace --all-targets
nix develop .#msrv --command cargo test --workspace
```

The contract validator checks the artifact inventory, JSON syntax and shape,
the C header's version declarations, and cross-artifact Substrait invariants.
Those invariants include the accepted Substrait version, Rust dependency pins,
extension URIs, planning state, and resource limits.

## License and dependency policy

Repository-authored source code, contract artifacts, and documentation are
licensed under Apache License 2.0. Cargo packages declare `Apache-2.0` through
workspace metadata. Third-party and native dependencies must retain their own
license notices and be reviewed before distribution; native libraries must not
be added to `connector-sdk` or source-specific connector crates.
