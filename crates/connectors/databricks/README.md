# Databricks connector

The Databricks connector reads Databricks through a SQL warehouse with the
ADBC Driver Foundry Databricks driver (ADBC access). Databricks is itself the
Data Source: the connector reads Unity Catalog through
`system.information_schema` and maps Databricks SQL types by its
[type mapping](#type-mapping), whatever format or federated source holds a
table's data.

The connector has not yet run against a workspace. What the driver does
was read from its source and its dependencies at the shipped version, and
what Databricks returns from its documentation and from the responses
databricks-sql-go records in its tests. What neither settles is marked as
to verify, and [Known limitations](#known-limitations) lists each item; the
[conformance suite](#tests) checks them once it runs.

## Data Source access

| Item | Value |
| --- | --- |
| ADBC driver | `adbc_driver_databricks`, the ADBC Driver Foundry Driver for Databricks, loaded through its driver manifest `adbc_driver_databricks.toml` in the library's ADBC driver directory |
| Supported driver versions | `>=0.4.0, <0.5.0` |
| Connection options | `host`, `http_path`, `port`, `access_token` (secret), `client_id`, `client_secret` (secret), and `catalog` |
| Fixed session settings | `SET TIME ZONE 'UTC'` and `SET ANSI_MODE = true`, run when a session opens |
| Fixed driver options | None |

`host` is the workspace's host name, without a scheme, port, or path, and
`http_path` the SQL warehouse's HTTP path, such as
`/sql/1.0/warehouses/<id>`, which chooses the warehouse. `port` is 443 by
default. The connector authenticates with exactly one of a personal access
token, `access_token`, or OAuth machine-to-machine authentication as a
service principal, `client_id` and `client_secret`, with the scope
`all-apis`; any other combination is `invalid settings`. The driver
refreshes the OAuth token. Azure Entra ID tokens and user-to-machine OAuth
are not offered.

`catalog` limits the namespace list and the table list to one Unity Catalog
catalog. It does not limit the table metadata request, and generated SQL
qualifies every table with its catalog, so it is not passed to the driver.
It is a setting of the Data Source.

The connection options map to the driver's options in one function,
`DatabricksOptions::driver_options`: `databricks.server_hostname`,
`databricks.http_path`, `databricks.port`, `databricks.auth_type` (`pat` or
`oauth-m2m`), `databricks.access_token`, `databricks.oauth.client_id`, and
`databricks.oauth.client_secret`. The driver has no option that limits
connecting, so the library's connection timeout does not reach it.

The connector runs the session settings as statements when a session
opens. Each ADBC connection of the driver holds one database/sql connection
for its lifetime, and databricks-sql-go opens one Thrift session for each
such connection, so one Databricks session runs every statement of an ADBC
connection and keeps the settings, whose scope Databricks documents as the
session. When databricks-sql-go reports the session gone, database/sql
closes that connection rather than replace it, so a later statement fails
instead of running in a session without the settings. The driver passes
session settings to the session's opening only from its `uri` option,
which excludes every other option and would carry the credentials inside a
URI, so the connector does not use it. The session time zone fixes the
time zone `timestamp` values arrive in; ANSI mode makes the expressions of
a view that is read raise errors instead of returning NULL. Generated
literals depend on neither. The driver itself asks for native Arrow
decimals, and databricks-sql-go for native timestamps and complex types,
leaving intervals to arrive as text.

The driver is identified by its driver manifest, which declares its name and
version, without `GetInfo`, as for every ADBC Driver Foundry driver
([Data Source access](../../../docs/connector-sdk.md)).

### Warehouse start and cost

Opening a session connects to the warehouse, which starts a stopped
warehouse: a serverless warehouse starts in seconds, a classic one in
minutes. Whether opening the session waits for it or the workspace answers
`503 Service Unavailable` while it starts is to verify: the driver makes no
HTTP retries unless told to, so a `503` fails opening the session at once
as a retryable `connection` error. Every metadata request runs
SQL on the warehouse, so listing, reading table metadata, and planning a
read also start the warehouse and keep it running, and billed, until it
stops itself after its auto-stop time.

### Cloud storage

The driver reads results through CloudFetch, which downloads them from
presigned cloud storage URLs, so the client must reach the workspace's cloud
storage, such as Azure Blob Storage or Amazon S3, as well as the workspace.
Both the workspace's requests and the downloads honour `HTTPS_PROXY`,
`HTTP_PROXY`, and `NO_PROXY`, read once, at the driver's first request.

## Dependencies

| Dependency | Used for | Ships with the connector | Tested versions |
| --- | --- | --- | --- |
| ADBC Driver Foundry Driver for Databricks (Apache-2.0) | Data Source access | Yes | None: no workspace has been tested |

The driver is one self-contained shared library, built from Go with
databricks-sql-go, Apache Thrift, Apache Arrow for Go, and Go's own TLS; its
bundled license file lists the licenses of what it contains. All are
permissive except `hashicorp/go-cleanhttp` and `hashicorp/go-retryablehttp`,
which are MPL-2.0; the package states where their source code is available
([Source code availability](../../../packaging/README.md#source-code-availability)).
The package and the development shell take it from the release's tarballs
on GitHub (`adbc-drivers/databricks`, tag `go/v0.4.0`). No build is
published for macOS x86_64. Exceptions: none. No dependency is installed by
the user.

## Source metadata

The source metadata of a table is the JSON document of `DatabricksTable`,
whose rustdoc defines its fields. It holds what Unity Catalog shows the
reading principal about the table:

| Item | From |
| --- | --- |
| `catalog`, `schema`, `name`, `table_type`, `data_source_format`, `comment` | `information_schema.tables` |
| `columns`: `name`, `ordinal_position`, `nullable`, `full_data_type`, `comment` | `information_schema.columns` |
| `primary_key`, `foreign_keys` | `information_schema.table_constraints`, `key_column_usage`, and `referential_constraints` |

Not read: owners, creation and alteration times and principals, and storage
paths, which change without a change of the table's definition; view
definitions; Delta CHECK constraints, which live in table properties; and
identifiers, which `information_schema` does not offer.

Table metadata derives from the document without settings:

- the kind from `table_type`: `MANAGED` and `MANAGED_SHALLOW_CLONE` are
  tables, `EXTERNAL` and `EXTERNAL_SHALLOW_CLONE` external tables, `VIEW`
  views, and every other type, such as `MATERIALIZED_VIEW`,
  `STREAMING_TABLE`, and `FOREIGN`, another kind in its own term;
- a column's ordinal is `ordinal_position`, which counts from 0, plus 1, and
  its nullability is `nullable`;
- primary and foreign keys are declared and never enforced, so they are
  returned as not enforced. A foreign key whose referenced key the principal
  cannot see leaves the constraints missing;
- no columns are hidden: Unity Catalog has no column privileges, and shows
  every column of a table it shows.

An example, `main.sales.orders`, built from `information_schema` rows as
the unit tests build them; no document has been read from a workspace
yet:

```json
{
  "catalog": "main",
  "schema": "sales",
  "name": "orders",
  "table_type": "MANAGED",
  "data_source_format": "DELTA",
  "comment": "All orders",
  "columns": [
    { "name": "id", "ordinal_position": 0, "nullable": false,
      "full_data_type": "bigint", "comment": "Order number" },
    { "name": "customer_id", "ordinal_position": 1, "nullable": true,
      "full_data_type": "bigint", "comment": null }
  ],
  "primary_key": { "name": "orders_pk", "columns": ["id"] },
  "foreign_keys": [
    { "name": "orders_customer_fk", "columns": ["customer_id"],
      "referenced": { "catalog": "main", "schema": "crm", "table": "customers",
                      "columns": ["id"] } }
  ]
}
```

The document's shape can change between connector versions; a stored
document is compared by restoring it to the `DatabricksTable` of the version
that compares it.

### Namespaces and tables

A namespace is a catalog and a schema, `[catalog, schema]`, and a table is
named by its catalog, schema, and name. A namespace name filter applies to
the schema name. `system.information_schema` spans every catalog of the
metastore, so each request runs one statement per `information_schema` view
whatever namespaces it names.

Not listed or read:

- the `system` catalog;
- every catalog's `information_schema` schema;
- the `hive_metastore` catalog, which `information_schema` does not
  describe;
- metric views, which a plain `SELECT` cannot read.

`information_schema` shows only the objects the principal holds a privilege
on, so a requested table it does not show is `not found`, never `does not
exist`, as is an excluded one. A table shown to a principal without
`SELECT` on it is read as metadata, and reading its rows is `permission
denied`. Unity Catalog stores every name but column names in lower case, so
a table named with upper-case letters is not found.

## Type mapping

Data Source types are read from `information_schema.columns.full_data_type`,
such as `decimal(10,2)`, `string collate UTF8_LCASE`, or
`array<struct<a:int,b:string>>`. Keywords are read in any case and with
any spacing. As Databricks' `STRUCT` syntax allows, a struct field name may
be quoted with backticks, its colon may be left out, and its type may be
followed by `NOT NULL` and `COMMENT`, which are dropped.

| Data Source type | Analytics type | Databricks' comparison | Runs remotely |
| --- | --- | --- | --- |
| `boolean` | `Boolean` | The same | `= <> < <= > >=` with a `Boolean` |
| `tinyint`, `smallint`, `int`, `bigint` | `Byte`, `SmallInt`, `Int`, `BigInt` | The same | Comparisons with the same type |
| `float`, `double` | `Float`, `Double` | The same: NaN equals NaN and is larger than any other value, and -0 equals 0 | Comparisons with the same type |
| `decimal(p,s)` | `Decimal(p, s)` | The same | Comparisons with a `Decimal` |
| `string`, `varchar(n)` | `Text` | The same under `UTF8_BINARY`, the default collation; other collations fold case or accents or ignore trailing spaces | Comparisons with `Text` under `UTF8_BINARY` |
| `char(n)` | `Text` | Values are padded with spaces to `n` | None |
| `binary` | `Blob` | The same | None |
| `date` | `Date` | The same | Comparisons with a `Date` from 0001-01-01 to 9999-12-31 |
| `timestamp` | `TimestampTz` | The same | Comparisons with a `TimestampTz` in years 1 to 9999 |
| `timestamp_ntz` | `Timestamp` | The same | Comparisons with a `Timestamp` in years 1 to 9999 |
| `array<T>` | `Array` of T's type, elements nullable | Not compared | None |
| `map<K,V>` | `Map` of K's and V's types, values nullable | Not compared | None |
| `struct<name:T,...>` | `Struct` of the fields' types, fields nullable | Not compared | None |

An array, map, or struct maps only when every type inside it maps;
otherwise its column is unsupported. Elements, values, and fields are
nullable because `full_data_type` does not state their nullability.
`decimal(p,0)` stays a decimal.

Not mapped: intervals, which only views can have; `variant`, whose typed
scalars `Json` would turn into strings; `geography`, `geometry`, `void`, and
the other types without an Analytics type. `timestamp_ntz` with 7 to 9
fractional digits is out of the mapping's range. Text that is no type the
connector reads is not mapped.

Whether a string column has a collation is read from `full_data_type`,
such as `string collate UTF8_LCASE`, because `information_schema.columns`
has no collation column; a string type that names no collation is taken as
`UTF8_BINARY`, the default collation. That a collated string's
`full_data_type` names its collation is to verify: Databricks documents
`full_data_type` as the type as specified in the column definition and
shows no collated example. If it does not, a collated column's comparisons
would run remotely with another meaning. The conformance suite reads such a
column's type and filters on it.

## Coverage

This version covers every mapping rule in its [type mapping](#type-mapping)
by converting the driver's output (`src/driver_output.rs`):

| Mapping rule | Driver output | Conversion |
| --- | --- | --- |
| `decimal(p,s)` | `Decimal128` of scale s | To the narrowest Arrow decimal of precision p, `Decimal32` up to 9 and `Decimal64` up to 18 |
| `timestamp` | `Timestamp` in microseconds labelled with the session's time zone, such as `Etc/UTC` | Labelled `UTC`; the instant is unchanged |
| `timestamp_ntz` | `Timestamp` in microseconds without a time zone | None |
| `array<T>`, `map<K,V>`, `struct<...>` | `List`, `Map`, `Struct` | Rebuilt with the Analytics fields, each element, key, value, or field converted by its own rule |

Every other mapped type arrives in its Analytics storage type: `float` as
`Float32`, `double` as `Float64`, `date` as `Date32`, `binary` as `Binary`,
and `string`, `varchar(n)`, and `char(n)` as `Utf8`, without their length.
The driver passes on the warehouse's Arrow IPC streams unchanged. The Arrow
schemas the warehouse returns, as databricks-sql-go's recorded responses
keep them, show these types; `timestamp_ntz` arrives without a time zone,
as Spark's Arrow conversion writes it. A column that arrives in another
form, such as a decimal of another scale or a struct whose fields differ,
fails the read as `internal`, naming the type that arrived.

No Databricks value is outside its mapped type's range: dates and timestamps
lie in years 1 to 9999, and decimals have at most 38 digits. A decimal that
arrives with more digits than its column's precision would be a `data`
error.

## Pushdown

| Offered work | Runs remotely when |
| --- | --- |
| Projection | Always, as plain columns |
| Filter | The whole filter is AND, OR, and NOT of the comparisons the Type mapping section lists and `IS [NOT] NULL`, each between a column and a constant or column of the same type; otherwise the whole filter stays local |

Literals state their type: integers with their suffix, decimals with their
scale, floating-point values with their suffix or as the cast of `NaN` and
the infinities, `timestamp` values with a UTC offset, and `timestamp_ntz`
values with none, so no session setting changes a comparison.

## Known limitations

The connector has not run against a workspace, so the conformance fixture
states no deviation yet. To verify against a workspace, each checked by the
suite where it can be:

- that `char(n)` values arrive padded with spaces;
- the `full_data_type` spellings, and that a collated string's names its
  collation, on which text pushdown under `UTF8_BINARY` depends;
- the HTTP statuses the error table reads: which of `401` and `403` the
  workspace answers a rejected personal access token with, since `403` is
  read as `permission denied`; that the OAuth token endpoint answers a
  rejected service principal with `invalid_client`; and what an HTTP path
  that names no warehouse is answered with, which is a retryable
  `connection` error unless it is `401` or `403`;
- whether opening a session waits for a stopped warehouse to start
  ([Warehouse start and cost](#warehouse-start-and-cost));
- whether `DESCRIBE TABLE ... AS JSON` offers a table identifier, which
  would let the connector offer source identifiers.

Errors are classified by the error classes and codes Databricks puts in its
messages, such as `TABLE_OR_VIEW_NOT_FOUND`, `INSUFFICIENT_PERMISSIONS`, and
`TEMPORARILY_UNAVAILABLE`, by their SQLSTATE, and by the text of the
driver's other failures, which it reports as `internal` with their cause
only in the text: a failure while connecting, and any HTTP request to the
workspace that failed, such as one whose connection broke, are
`connection` errors, and make the session unusable. A message keeps only
its first line, and a syntax error's message loses its text, since
Databricks quotes the statement there. Any other error the driver reports
in the middle of a result stream reaches the connector as text only, so it
is `data-source`.

A dropped table is `not found`, not `does not exist`: Unity Catalog reports
a table the principal holds no privilege on the same way.

The driver stops a statement at the warehouse only while it waits for the
statement to finish, by sending `CancelOperation` when its call is
cancelled; the statement has finished at the warehouse before the first
batch is read. Cancelling a read or letting it expire therefore stops the
statement while it runs, and afterwards ends the read at its next pull. A
pull already waiting for a result page or a CloudFetch download completes
that batch first, since the driver cannot interrupt it; releasing the read
then closes the operation with `CloseOperation` and aborts the downloads
still in flight. A cancel that comes while the request sending the
statement is in flight ends the read but cannot reach the statement, which
runs to its end at the warehouse; one that comes the instant before the
driver starts executing the statement is lost to the driver, so the read
returns the stop only once the statement has finished.

The library's connection timeout does not limit opening a session, as the
contract requires: the driver has no option for it, so opening a session
waits as long as the driver does. The driver gives up connecting a TCP
connection after 30 seconds and a TLS handshake after 10, makes no HTTP
retries, and waits for each request's response up to 15 minutes; its
check of the new session waits up to 60 seconds. A host that accepts the
connection and never answers therefore fails after about 10 seconds, and
one that completes the TLS handshake and never answers after 15 minutes.

A warehouse that accepts only the Statement Execution API and rejects the
Thrift protocol, such as a real-time warehouse, cannot be read: the
shipped driver is built without the databricks-sql-go backend that reads
such warehouses, and opening a session fails.

Metadata requests cannot be made to wait at Databricks: Unity Catalog's
`information_schema` takes no locks a reader could wait for. The suite's
checks that a cancelled or expired metadata request stops at the source
therefore do not run for this connector; cancellation and timeouts of
reads, which use the same driver cancellation, are checked.

The `hive_metastore` catalog is not read, because `information_schema` does
not describe it. Reading a table is one `SELECT`, which reads one snapshot
of a Delta or Iceberg table; views over several tables and foreign tables
have no such guarantee.

## Compatibility

Only combinations that passed this crate's tests are listed.

| Databricks | ADBC driver | Platform |
| --- | --- | --- |

No workspace has been tested.

## Tests

The unit tests need no driver, network, or workspace: they cover the
options, the `full_data_type` grammar and type mapping, the
`information_schema` statements and the conversion of their rows, the
rendered SQL, pushdown, the error table, and the conversion of the driver's
output from hand-built Arrow arrays. `tests/driver.rs` loads the driver from
`ADBC_DRIVER_DIRECTORY`, which the Nix development shell sets, and opens
sessions against `127.0.0.1`, on a closed port and on a listener that closes
every connection, checking that the failures are `connection` errors and
reveal no credential; it is skipped when the directory is not set. Neither
needs Docker:

```sh
nix develop --command cargo test -p connector-databricks
```

`tests/conformance` runs the SDK's connector conformance suite against a
Databricks workspace with a SQL warehouse; without these variables it is
skipped, saying which are missing:

| Variable | Value |
| --- | --- |
| `DATABRICKS_TEST_HOST` | The workspace's host name |
| `DATABRICKS_TEST_HTTP_PATH` | The SQL warehouse's HTTP path |
| `DATABRICKS_TEST_CATALOG` | A catalog the fixture creates its schemas in |
| `DATABRICKS_TEST_TOKEN` | A personal access token, or instead: |
| `DATABRICKS_TEST_CLIENT_ID`, `DATABRICKS_TEST_CLIENT_SECRET` | A service principal's OAuth client ID and secret |

The principal needs `USE CATALOG` and `CREATE SCHEMA` on the catalog and
"Can Use" on the warehouse. The fixture creates two schemas with a unique
suffix, loads its tables with the SQL Statement Execution API, and drops
both schemas when the test ends, also when it fails. A run starts the
warehouse and runs queries that take minutes until the suite cancels them,
so it costs warehouse time.

The fixture observes whether a query still runs through the query history
API, filtered by the run's schema name. Its waiting read is a view that
counts a hundred billion generated rows in one task, and its long read a
view of that many rows. It breaks connections with a local HTTPS CONNECT
proxy that `HTTPS_PROXY` names: the driver checks the workspace's
certificate against its host name, so a proxy cannot stand in for the host.
The Go runtime reads the environment when the driver loads, so the test runs
the suite in a child process with `HTTPS_PROXY` set. One credential cannot
show permission denied, so that error is not covered.
