# Reach ODBC sources through direct access

## Design question

Should Generic ODBC Connectors reach their sources through an ADBC driver that
uses ODBC, or through direct access that calls ODBC from the connector crate?

## Decision

Generic ODBC Connectors use direct access. The connector crate calls the ODBC
driver manager through `odbc-api` and converts result sets to Arrow with
`arrow-odbc`. The ODBC driver manager is a native dependency the connector
links dynamically, which the guideline of keeping native dependencies in ADBC
drivers does not cover; the connector records it as an exception
([Which dependencies a connector may use](../connector-implementation.md#which-dependencies-a-connector-may-use)).
The source's ODBC driver stays outside the library, loaded at run time by the
driver manager.

## Rationale

Native dependencies are kept in ADBC drivers so that the library does not carry
vendor code and a source's client is loaded only when a connector needs it. For
ODBC the driver manager already does this: it is a loader that finds the
source's ODBC driver from the connection string at run time. Putting an ADBC
driver in front of it adds a second loader with the same purpose:

~~~text
ADBC access:   connector -> ADBC driver manager -> ADBC driver -> ODBC driver manager -> ODBC driver
direct access: connector ----------------------------------------> ODBC driver manager -> ODBC driver
~~~

The extra layer also hides what a Generic ODBC Connector needs most. In this
tier Data Source types and metadata follow the ODBC driver's report
([Support tiers](../connector-contract.md#support-tiers)), so the connector
needs the ODBC SQL type, precision, and scale of each column and the ODBC
catalog functions (`SQLTables`, `SQLColumns`, `SQLPrimaryKeys`,
`SQLForeignKeys`). Through ADBC it receives only the Arrow type the driver
chose, and catalog data only after the driver has re-encoded it into the
nested `GetObjects` result. Direct access lets the connector read the column
descriptions and catalog functions itself and pass `arrow-odbc` the Arrow
schema derived from its type mapping, so the connector alone decides types.

Errors and cancellation are also more faithful without ADBC in between:

- The Rust ADBC driver manager does not import `AdbcErrorFromArrayStream`
  ([apache/arrow-adbc#4984](https://github.com/apache/arrow-adbc/issues/4984)),
  so an error while reading reaches the connector as a message without its
  SQLSTATE.
- In the ADBC 1.1 error layout the vendor code is replaced by the details
  sentinel, so the ODBC native error code must travel as an error detail.
- Cancellation must go through a cancel handle that ADBC creates once per
  statement, although ODBC allocates a statement handle per query.

With direct access the connector classifies `odbc-api` errors with their
diagnostic records, and cancels the statement handle of the running query
itself.

`arrow-odbc` already performs the reading: binding column buffers, fetching in
blocks, converting to Arrow, and detecting values that would be truncated. An
ADBC driver around it adds the C ABI, its error and stream wrappers, and a
manifest, without adding behavior the connector needs.

## Alternatives not taken

- **An ADBC driver that uses ODBC**, built in this repository with `adbc_core`,
  `adbc_ffi`, and `arrow-odbc`. It was implemented and works, but it has the
  costs above: a second loader, lost column descriptions and catalog data, a
  reimplemented `AdbcErrorFromArrayStream`, and read errors that still lose
  their SQLSTATE through the Rust driver manager.
- **The same ADBC driver in a Cargo workspace of its own**, so that the
  library's minimum Rust version and dependency policy stay untouched. It keeps
  every cost of the ADBC driver.
- **An existing ADBC driver for ODBC.** The only one found, adbcbridge, is a
  third-party 0.1.x release whose maturity could not be established.

## Consequences

- A library built with a Generic ODBC Connector's feature links the ODBC
  driver manager (unixODBC on Linux and macOS, LGPL-2.1) dynamically and fails
  to load where it is absent; a build without that feature is unaffected. The
  connector records the driver manager as an exception in `deny.toml` and in
  its README, and ships it as
  [Shipping and installing dependencies](../connector-implementation.md#shipping-and-installing-dependencies)
  describes. If connectors are later loaded dynamically instead of being linked
  into the library, this dependency moves to the ODBC connector's own artifact.
- The source's ODBC driver is a dependency the connector declares, shipped or
  installed by the user as its license allows.
- `odbc-api` and `arrow-odbc` require Rust 1.88, so the workspace's minimum
  Rust version rises to 1.88 when the connector is added. `arrow-odbc` enables
  `odbc-api`'s default `prompt` feature, which brings `winit` into the library,
  and depends on `encoding_rs`, whose license includes BSD-3-Clause; the
  connector records that license as an exception too.
- Generic ODBC Connectors share the SQL-family processing of `connector-sql`,
  so `connector-sql` must not assume ADBC access.
