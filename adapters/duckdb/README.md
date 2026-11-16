# DuckDB adapter

The DuckDB Query Engine adapter attaches each Data Source to DuckDB as one
read-only catalog and plans every scan of its tables through the connector
API's pushdown planning. It is a C++ DuckDB extension built with CMake against
DuckDB v1.5.6, the version the Nix development shell provides; the adapter
uses DuckDB's C++ API, so a binary works only with that DuckDB version.

The adapter follows the
[adapter implementation conventions](../../docs/adapter-implementation.md).
This document records what is specific to DuckDB.

## Attaching Data Sources

A Data Source attaches with the `scalardb_analytics_connectors` type. The path
names the Connector, and every other option is a connection option of that
Connector.
Each option has the value type the Connector declares in the library's static
information: a string, a 64-bit integer, or a boolean. A value of another type
is rejected rather than converted, so `port '5432'` fails where `port 5432` is
accepted, and an option the Connector does not declare fails the ATTACH:

```sql
ATTACH 'postgresql' AS pg (TYPE scalardb_analytics_connectors,
    host '127.0.0.1', port 5432, database 'postgres', user 'postgres', password '...');
SELECT * FROM pg.public.orders WHERE id = 1;
```

A DuckDB secret of the `scalardb_analytics_connectors` type holds one Data
Source's connection options instead, so that the ATTACH carries none of them.
Each
Connector is a provider of the type, whose parameters are the options the
Connector declares. DuckDB casts each value to the declared type when it
creates the secret, so there `port '5432'` is accepted, and an option the
Connector does not declare fails. The options the Connector declares secret,
such as a password, are redacted in `duckdb_secrets()`. An ATTACH with
`SECRET` takes its Connector and every connection option from the secret: it
takes no other option, and its path is empty or the secret's Connector. A
credential that the application binds to a variable never appears in any
statement text or in DuckDB's query log:

```sql
SET VARIABLE pw = ?;  -- bound by the application
CREATE TEMPORARY SECRET pg_source (TYPE scalardb_analytics_connectors,
    PROVIDER postgresql, host '127.0.0.1', port 5432, database 'postgres',
    user 'postgres', password getvariable('pw'));
RESET VARIABLE pw;
ATTACH '' AS pg (TYPE scalardb_analytics_connectors, SECRET pg_source);
```

Whether secrets may persist is DuckDB's setting (`allow_persistent_secrets`);
a persistent secret is stored by DuckDB, not by the adapter.

The catalog's schemas are the Connector's readable namespaces, named by their
name path joined with dots, such as `test.public`; a name containing a dot or
a double quote is double-quoted inside it, as in `test."a.b"`, so that distinct
name paths never share a schema name. Its tables are their
tables, with the columns that have an Analytics type. A column without one
(unsupported, or its type not read) is a virtual column: `*` and `DESCRIBE`
leave it out, naming it fails with the reason it has no Analytics type, and
the table's comment (`duckdb_tables()`) lists such columns with their
reasons. A table has no `rowid`. Schema and table names resolve as DuckDB
identifiers do: an exact match, or else the one name that differs only in
case. A name the lists do not hold is described by its own request before it
is reported missing, so a namespace or table the Connector could not read
fails with the Connector's reason rather than as one that does not exist. The
lists, which DuckDB's enumeration such as `information_schema` shows, hold
only what the Connector could read. Namespaces, tables, table metadata, and
names described as missing are read when DuckDB first looks them up and are
kept for the life of the attached catalog, because DuckDB's
catalog entries must outlive the lookups that return them; to see changes in
the source, DETACH and ATTACH again. One exception is a table the Connector
reports as a stale registration (`does not exist` or `schema mismatch`), when
planning or reading finds that its shape no longer matches the source: the
failing query reports it with that kind, and the next query that uses the table describes it again. The
replaced entry is kept, since a bound query may still use it. Every Connector
error names its kind, as the contract does. Writes and DDL fail. ANALYZE and
VACUUM fail because DuckDB keeps statistics only for its own tables, but only
after reading the whole table. ATTACH fails when the
database disables external access (`enable_external_access`).

Three DuckDB settings limit how long a request to a Data Source may take, in
milliseconds; 0, the default, sets no limit:

| Setting | Limits | Read when |
| --- | --- | --- |
| `scalardb_analytics_connection_timeout` | Connecting while the Data Source and its sessions open, which cannot be cancelled | the Data Source is attached |
| `scalardb_analytics_metadata_timeout` | Each catalog request: namespace and table lists and descriptions | the Data Source is attached |
| `scalardb_analytics_read_timeout` | Each read, from its first row to its end | a scan starts |

A request over its limit fails with the `timeout` kind. Interrupting a DuckDB
query does not cancel a request already sent to the Data Source; it ends when
the source answers or its limit passes. Planning is limited by the library
itself.

`PRAGMA scalardb_analytics_attach('<file>')` attaches every Data Source a JSON
definitions file lists, each as its own catalog as an ATTACH does. It attaches
them directly rather than through generated ATTACH statements, so the
connection options never appear in DuckDB's query log:

```json
[
  {"name": "pg", "connector": "postgresql", "options": {"host": "127.0.0.1", "password": "..."}},
  {"name": "sales", "connector": "mysql", "options": {"host": "..."}}
]
```

Each entry has exactly a non-empty string `name` and `connector` and an
optional `options` object whose values are strings, integers, or booleans;
names are unique ignoring
case, and an option may not be one DuckDB takes for itself in ATTACH (`type`,
`read_only`, `readonly`, `read_write`, `readwrite`, `hidden`, `default_table`,
`recovery_mode`). A file that breaks any of these is rejected with a message
that names the entry and key but no value. Either every entry is attached or
none is: when one fails to attach, the ones attached before it are detached.

The file stands in for the list of registered Data Sources while no catalog
service provides it. Its connection options, including credentials, are plain
text. DuckDB resolves a PRAGMA while it parses a batch of statements, so the
pragma runs in a batch after the one that loads the extension, not in the same
batch.

## Entry points

The loadable extension, `scalardb_analytics_connectors.duckdb_extension`, owns the
connector API library unless another caller in the process already does: it
initializes the library when DuckDB loads it and never shuts it down, because
DuckDB does not unload extensions. It reads the
ADBC and ODBC driver directories from the environment variables
`SCALARDB_ANALYTICS_ADBC_DRIVER_DIRECTORY` and
`SCALARDB_ANALYTICS_ODBC_DRIVER_DIRECTORY`, because DuckDB applies an
extension's own settings only after loading it. Without a variable, it uses
the `adbc-drivers` or `odbc-drivers` directory of the
[distribution package](../../README.md#distribution-package) it is in, beside
its own directory. A DuckDB setting cannot name a directory, because any SQL
user who can set it could make the process load a native library. The
extension is unsigned, so DuckDB loads it only with unsigned extensions
allowed.

When the library is already initialized as DuckDB loads the extension, the
extension uses it as it is. This serves an application that owns the library
but runs DuckDB through a binding that can only load extensions, such as a
JDBC or Python driver: the application initializes the library with its own
settings, shares it with its other callers, and loads the extension into
DuckDB. The extension's driver directories are then not used, and the application
shuts the library down only after DuckDB has closed the catalogs the extension
attached.

An application that embeds DuckDB through its C++ API and owns an initialized
library can use
[`scalardb_analytics_duckdb.hpp`](include/scalardb_analytics_duckdb.hpp)
instead: `Register` adds the attach type and the pragma to a database without
touching the library's lifecycle.

## Catalog sources

A Query Engine reads table metadata through one of two paths
([Catalog paths](../../docs/architecture.md#catalog-paths)): through the
Physical Catalog, or directly from the Connector. The adapter implements the
direct path and keeps the other one replaceable:

| Part | Responsibility |
| --- | --- |
| `CatalogSource` | Lists namespaces and tables, and describes one namespace (`NamespaceMetadata`) or one table (`TableMetadata`, the connector API structures) |
| `ConnectorCatalogSource` | The direct path, over `namespace_list`, `table_list`, and `table_metadata`; a namespace is described by a list filtered to its name |
| `CreateCatalog` | Builds the DuckDB catalog of one Data Source from any catalog source |

A catalog source returns table metadata with the means to release it, and the
adapter owns it from then on: it releases the metadata when the table's
catalog entry is dropped. A component that holds registered metadata, such as
a Physical Catalog client, implements `CatalogSource`, opens the
`DataSource`, and calls `CreateCatalog` from its own attach function; it
depends on this adapter, not the reverse.

## Planning a scan

DuckDB's filter pushdown optimizer calls the scan's `pushdown_complex_filter`
for every scan, with or without filters. At its first call the adapter offers
the scan's region as one complete request: a bare read of the table, the
pushed filters Substrait can express, and the columns the query references. A
filter it cannot express, such as one calling a DuckDB function, stays in
DuckDB. A comparison is expressed only when the [type mapping](#type-mapping)
offers it for its operands' type; an offered comparison has its Analytics
meaning, and one a Connector keeps in the local plan is lowered back to
DuckDB's comparison.

DuckDB may call again after a region was adopted, as its CTE filter pusher
does for a materialized CTE. The adopted region has already taken its filters
out of DuckDB, so planning again with only the new filters would lose them;
the new filters stay in DuckDB. When the filter pushdown optimizer is
disabled, DuckDB never calls, and the scan plans a read of the columns it is
asked for when it starts, with every filter left in DuckDB.

```text
pushdown_complex_filter(filters)
  request 1: ReadRel -> FilterRel(expressible filters) -> ProjectRel(columns)
    adoptable: record the result in the scan and remove the covered filters
  request 2: ReadRel -> ProjectRel(columns), every filter left in DuckDB
    adoptable: record the result
  neither:   nothing changes; the scan fails at execution with the reasons
```

A result is adoptable when the remote output is the shape the adapter reads
and its local plan lowers to DuckDB expressions that produce the requested
columns. Adoption changes the scan's bind data and DuckDB's filter list
together; an unsupported result, an error, or a result that cannot be lowered
changes neither, and its remote execution is freed. A later call, with filters
DuckDB pushes further down, keeps an adopted result and leaves those filters in
DuckDB. A scan whose region was never offered, as when DuckDB's optimizer is
disabled, plans the read of its columns alone when it starts.

Execution starts a read of the adopted remote execution, checks the stream's
Arrow schema against the planned remote output, converts batches with DuckDB's
Arrow reader, and evaluates the local filter and projection in the scan.

Joins, aggregation, and other regions need a DuckDB optimizer extension and
are not offered.

## Type mapping

Each Analytics type maps to one DuckDB type
([Execution](../../docs/analytics-types.md#execution)). The offered column
lists the comparisons the adapter offers between two operands of the type, a
column and a constant or two columns; the last column states how DuckDB's
comparison of the mapped type differs from the
[Analytics comparison](../../docs/analytics-types.md#comparison):

| Analytics type | DuckDB type | Loss | Offered | DuckDB's comparison |
| --- | --- | --- | --- | --- |
| Boolean | BOOLEAN | Lossless | Equality and ordering | The same |
| Byte, SmallInt, Int, BigInt | TINYINT, SMALLINT, INTEGER, BIGINT | Lossless | Equality and ordering | The same |
| UnsignedByte, UnsignedSmallInt, UnsignedInt, UnsignedBigInt | UTINYINT, USMALLINT, UINTEGER, UBIGINT | Lossless | Equality and ordering | The same |
| Float, Double | FLOAT, DOUBLE | Lossless | Equality and ordering | The same: NaN equals NaN and orders after every other value, and -0 equals +0 |
| Decimal(p, s), p ≤ 38, s ≥ 0 | DECIMAL(p, s) | Lossless | Equality and ordering | The same |
| Decimal(p, s), p ≤ 38, s < 0 | DOUBLE | DuckDB has no negative scale; the value becomes an approximate floating-point number. Inside an Array, Map, or Struct it is not mapped, and a table with such a column cannot be bound | None | Compares the approximate numbers |
| Decimal(p, s), p > 38 | Not mapped | DuckDB decimals hold at most 38 digits. The column can be named but not read; reading it fails with the reason | None | — |
| Text | VARCHAR | Lossless | Equality and ordering | The same without a collation; a collated comparison binds through a collation function, which is not offered |
| Blob | BLOB | Lossless | Equality and ordering | The same |
| Date | DATE | Lossless | Equality and ordering | The same |
| Time | TIME | Lossless | Equality and ordering | The same; DuckDB also holds `24:00:00`, which a Time cannot, so a comparison with that constant is not offered |
| Timestamp | TIMESTAMP | Lossless | Equality and ordering | The same |
| TimestampTz | TIMESTAMP WITH TIME ZONE | Lossless | Equality and ordering | The same |
| Duration | INTERVAL | The microseconds are kept; INTERVAL arithmetic is not that of a fixed-length duration | None | The same: an INTERVAL of microseconds alone compares by length |
| Interval | INTERVAL | Nanoseconds are truncated to microseconds | None | Compares after normalizing 24 hours to a day and 30 days to a month, so 1 month equals 30 days, and orders intervals |
| Array(T) | LIST of T's type | As T | None | The Analytics types define no comparison |
| Map(K, V) | MAP of K's and V's types | As K and V | None | The Analytics types define no comparison |
| Struct(...) | STRUCT of the fields' types | As each field | None | The Analytics types define no comparison |
| Uuid | UUID | Lossless | Equality | The same equality |
| Json | JSON | Lossless; the text is kept as DuckDB's JSON type | Equality | The same equality: the text is compared |
| Enum | ENUM of the members | Lossless | Equality, also with a string constant | The same equality; DuckDB binds an Enum column compared with a string as the column cast to VARCHAR, which is offered as Enum equality when the string is a member and not offered otherwise |
| Year | SMALLINT | The meaning of a year is lost | Equality | The same equality |
| TimeWithTimeZone | VARCHAR | The time-of-day meaning is lost and the text is kept, because DuckDB's TIMETZ compares instants rather than time and zone | Equality | The same equality: the text compares time and zone |

DuckDB also orders the values of the types the Analytics types do not order,
such as Enum members by declaration; those comparisons are not offered. The
Uuid and TimeWithTimeZone rows depend on their Arrow storage, which may still
change.

## Building and testing

The connector API library is built first, with the Connectors the tests open;
CMake links the extension and the tests against it. The PostgreSQL tests start
a PostgreSQL server in Docker, as the PostgreSQL Connector's own tests do.

```sh
nix develop --command cargo build -p connector-api --features test-connector,postgresql
nix develop --command cmake -S adapters/duckdb -B target/duckdb-adapter -G Ninja
nix develop --command cmake --build target/duckdb-adapter
nix develop --command ctest --test-dir target/duckdb-adapter --output-on-failure
```

`ctest` runs three suites. The integration tests embed DuckDB through
`Register`, with a library the test process initialized. The extension test
initializes the library first and then loads the built extension, as an
application that owns the library does. The end-to-end test,
[`test/e2e.sh`](test/e2e.sh), runs the DuckDB CLI of the development shell,
loads the built extension, which initializes the library itself, and queries
PostgreSQL as a user would: complete and partial pushdown with their plans, an
aggregate, a join with a DuckDB table, a rejected write, and the bulk-attach
pragma and an ATTACH through a secret, each with the query log checked for the
password.

The extension of the distribution package is built with
`-DSCALARDB_ANALYTICS_DISTRIBUTION=ON` by `packaging/package.sh`: CMake then
builds only the extension, finds the connector API library in `../lib` beside
its own directory, and links DuckDB, protobuf, and abseil statically. It takes
DuckDB's static library from the DuckDB release, and DuckDB's headers,
protobuf, abseil, and the Substrait specification from pinned source archives,
instead of the development shell. The extension exports only its entry point,
so a process that embeds DuckDB v1.5.6 loads it even when it exports none of
DuckDB's C++ symbols, as DuckDB's JDBC driver does. The development build
links the static library of the development shell's DuckDB the same way, so
the end-to-end test loads the extension as a user's process does. The
adapter has no version of its own; CMake reads the workspace version from
Cargo.

The integration tests embed DuckDB through the embedding entry. Against the
items of [Integration tests](../../docs/adapter-implementation.md#integration-tests):

| Item | Coverage |
| --- | --- |
| Complete pushdown; partial pushdown with intermediate columns | PostgreSQL cases, including one query with a filter pushed to PostgreSQL and a filter kept local, and the end-to-end test through the loadable extension |
| Unlowerable local plan, then another request adopted; unsupported results and errors; the DuckDB rollback case | Test Connector cases |
| Fully-local fallback; read, projection, and filter offered as one region; execution of the adopted remote execution | Test Connector cases, over rows that each filter tells apart. Execution covers one batch larger than DuckDB's vector size, a value of every Analytics type with its mapped type, a scan the optimizer never offers, and a later callback. The local plan runs inside the scan over its one remote stream, so no case can observe a second read |
| Every offered plan passes validation | A test Connector case over a table of every Analytics type kind, one condition per query on each comparable one, each stating whether it is offered |
| Connector-introduced expressions | Not covered: no Connector produces a local expression beyond the offered filter and column selections yet |
| The first scan initializes the exact base `ReadRel` before later attempts | Not applicable: a scan makes its attempts in one callback, each from the base `ReadRel` built from the table metadata |
| Alternative join shapes | Out of scope: the adapter offers no join regions, which need an optimizer extension |
| Native handle persistence | Out of scope: DuckDB plans and executes in one process |
