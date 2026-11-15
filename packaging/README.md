# Packaging

This directory builds and checks the
[distribution package](../README.md#distribution-package): one tgz per
platform that works wherever it is unpacked.

| File | Role |
| --- | --- |
| `dependencies.json` | What ships: the connectors built into the library and the dependencies each one ships |
| `package.sh` | Builds the package of the running platform from that declaration |
| `check-package.sh` | Checks an unpacked package: binary references, and the DuckDB e2e |
| `linux/package.sh` | Runs `package.sh` for Linux in the pinned manylinux_2_28 image with Docker |
| `linux/build.sh` | Prepares the manylinux container and runs `package.sh` in it |
| `about.toml`, `about.hbs` | cargo-about configuration and template for the Rust crates' license texts |

## What ships

`dependencies.json` has two parts:

- `connectors`: each key is a `connector-api` feature. The library is built
  with exactly these features, and its Rust license texts cover them.
- `dependencies`: each entry has a `kind` and the fields that kind needs.
  A connector lists the entries it ships under `dependencies`; an entry
  shared by several connectors ships once.

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

## Adding a connector

1. Add the connector's `connector-api` feature under `connectors`, with the
   dependencies it ships.
2. Declare each new dependency under `dependencies` with an existing kind, or
   add a kind as below.
3. Build and check the package on each platform. `check-package.sh` rejects a
   shipped binary that references a library which is neither a system library
   nor in the package's `lib/`: such a library is a dependency that is not
   declared, or is placed where nothing finds it.

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
