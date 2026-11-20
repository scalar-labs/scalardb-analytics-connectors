# Databricks connector

The Databricks connector reads Databricks through a SQL warehouse with an
ADBC driver (ADBC access). Databricks is itself the Data Source: the
connector reads Unity Catalog through `system.information_schema` and maps
Databricks SQL types by its [type mapping](#type-mapping), whatever format
or federated source holds a table's data.

This version is not complete. The connector's options, type mapping, Unity
Catalog reading, SQL, pushdown, and error table are implemented and tested
without a workspace. Not yet decided or done: which ADBC driver it ships
with, how the driver's Arrow output converts to Analytics values, the fixed
session settings, and the conformance fixture, which needs a workspace. The
sections below mark each item that waits on them.

## Data Source access

| Item | Value |
| --- | --- |
| ADBC driver | `adbc_driver_databricks`, the ADBC Driver Foundry Driver for Databricks, loaded through its driver manifest `adbc_driver_databricks.toml` in the library's ADBC driver directory. Not yet decided: see [Known limitations](#known-limitations) |
| Supported driver versions | `>=0.4.0, <0.5.0` |
| Connection options | `host`, `http_path`, `port`, `access_token` (secret), `client_id`, `client_secret` (secret), and `catalog` |
| Fixed session settings | To be run on each connection: `SET TIME ZONE 'UTC'` and `SET ANSI_MODE = true`. Not yet run |
| Fixed driver options | None |

`host` is the workspace's host name, without a scheme, port, or path, and
`http_path` the SQL warehouse's HTTP path, such as
`/sql/1.0/warehouses/<id>`, which chooses the warehouse. `port` is 443 by
default. The connector authenticates with exactly one of a personal access
token, `access_token`, or OAuth machine-to-machine authentication as a
service principal, `client_id` and `client_secret`; any other combination is
`invalid settings`. The driver refreshes the OAuth token.

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

Opening a session connects to the warehouse, which starts a stopped
warehouse: a serverless warehouse starts in seconds, a classic one in
minutes. Every metadata request runs SQL on the warehouse, so it too starts
the warehouse and keeps it running until it stops itself.

The driver reads results through CloudFetch, which downloads them from
presigned cloud storage URLs, so the client must reach the workspace's
cloud storage as well as the workspace.

## Dependencies

| Dependency | Used for | Ships with the connector | Tested versions |
| --- | --- | --- | --- |
| ADBC Driver Foundry Driver for Databricks (Apache-2.0) | Data Source access | Not yet: the driver is not decided | None |

The driver is one self-contained shared library built from Go, as the MySQL
connector's driver is, with no native dependency. It is not yet in the
package's dependency declaration or the development shell. Exceptions: none.

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
the tests build them; no document has been read from a workspace yet:

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
`array<struct<a:int,b:string>>`; a struct field name may be quoted with
backticks.

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
because `information_schema.columns` has no collation column; a string
type that names no collation is taken as `UTF8_BINARY`. This must be
checked against a workspace before text comparisons run remotely in a
release.

## Coverage

No mapping rule is covered yet: the connector has not read a workspace, and
it passes the driver's output unchanged, so a column whose Arrow type
differs from its Analytics storage fails the read as `internal`. Which
conversions are needed, such as `Decimal128` to the narrower decimals for
precision up to 18, the time zone of `timestamp` values, and the
nullability inside nested types, is checked with the chosen driver.

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

Not yet decided:

- the driver: the ADBC Driver Foundry Go driver, or a driver of our own over
  the SQL Statement Execution API. Both are ADBC drivers, so only the
  driver requirement and `DatabricksOptions::driver_options` change;
- whether metadata may run SQL on the warehouse or should be read from the
  Unity Catalog REST API. `information_schema` is one module behind the
  catalog functions, which a REST source would replace.

Not yet done: running the fixed session settings, the conversion of the
driver's output, the driver's packaging, and the conformance fixture.

Errors are classified by the error classes and codes Databricks puts in its
messages, such as `TABLE_OR_VIEW_NOT_FOUND`, `INSUFFICIENT_PERMISSIONS`, and
`TEMPORARILY_UNAVAILABLE`, and by their SQLSTATE; the messages the driver
passes on are checked with the chosen driver. A message keeps only its
first line, and a syntax error's message loses its text, since Databricks
quotes the statement there.

## Compatibility

Only combinations that passed this crate's tests are listed.

| Databricks | ADBC driver | Platform |
| --- | --- | --- |
| None yet | | |

## Tests

The unit tests need no driver, network, or container: they cover the
options, the `full_data_type` grammar and type mapping, the
`information_schema` statements and the conversion of their rows, the
rendered SQL, pushdown, and the error table.

```sh
nix develop --command cargo test -p connector-databricks
```

The conformance fixture needs a Databricks workspace with a SQL warehouse
and is not written yet.
