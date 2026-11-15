# PostgreSQL connector

The PostgreSQL connector reads PostgreSQL through the Apache Arrow ADBC
PostgreSQL driver (ADBC access). It reads `pg_catalog` itself and maps types by
its [type mapping](#type-mapping). The
general conventions it follows are in
[Initial PostgreSQL and MySQL conventions](../../../docs/connector-implementation.md#initial-postgresql-and-mysql-conventions).

## Data Source access

| Item | Value |
| --- | --- |
| ADBC driver | `adbc_driver_postgresql`, loaded through its driver manifest `adbc_driver_postgresql.toml` in the library's ADBC driver directory |
| Supported driver versions | `>=1.13.0, <1.14.0` |
| Connection options | `host`, `port`, `database`, `user`, `password` (secret), `sslmode`, and `search_path` |
| Fixed session settings | `TimeZone=UTC`, `client_encoding=UTF8`, and `standard_conforming_strings=on` |
| Fixed driver options | `adbc.postgresql.use_copy=true` and `adbc.postgresql.type_resolver_mode=server` |

The driver reports its version as `unknown`, so the version comes from the
driver manifest.

`search_path` decides how Data Source types are spelled: `format_type`
qualifies a user-defined type, such as an enum or a domain, with its schema
unless the type is on the path. Without the option, the server's setting for
the role and database applies.

## Dependencies

| Dependency | Used for | Ships with the connector | Tested versions |
| --- | --- | --- | --- |
| Apache Arrow ADBC PostgreSQL driver (Apache-2.0) | Data Source access | Yes | 1.13.0 |

Exceptions: none. No dependency is installed by the user.

## Source metadata

The source metadata of a table is the JSON document of `PostgresTable`. It
holds what `pg_catalog` states about the table: the facts its table metadata
derives from, and what states the table's meaning for the Semantic Catalog,
such as comments, view definitions, and how columns are filled. The rustdoc of
`PostgresTable` and the types it contains defines each field.

A document describes one table, so its top level is the table. It follows the
concepts a reader works with: each fact appears once, with the table, column,
or type it belongs to, and kinds are named rather than given as catalog codes.
Types are referred to by their display names.

| Item | From |
| --- | --- |
| Table: `namespace`, `name`, `oid` | `pg_class`, `pg_namespace` |
| Table: `kind` | `relkind`: `table`, `partitioned table`, `view`, `materialized view`, or `foreign table` |
| Table: `comment` | `obj_description(oid, 'pg_class')` |
| Table: `definition`, for views and materialized views | `pg_get_viewdef(oid)` |
| Table: `primary_key`, `unique_keys`, `foreign_keys`, `checks` | `pg_constraint`, and for `unique_keys` also the unique indexes from `pg_index` that back no constraint, with expression keys from `pg_get_indexdef` |
| Column: `name`, `attnum`, `typmod` | `pg_attribute` |
| Column: `not_null` | The column's own `NOT NULL` (`attnotnull`), with `validated` from its `pg_constraint` row on PostgreSQL 18, where it can be `NOT VALID` |
| Column: `data_source_type` | `format_type(atttypid, atttypmod)` |
| Column: `type` | `format_type(atttypid, NULL)`, the display name of one of `types` |
| Column: `comment` | `col_description(attrelid, attnum)` |
| Column: `value_source` | `attidentity`, `attgenerated`, and the `pg_attrdef` expression: a default, an identity column, or a generated column |
| `types` | Each `pg_type` row the columns lead to through domains' base types and arrays' element types, once, in display name order, with its display name, `schema`, and `typname`: a `base` type, an `array` with its `element`, a `domain` with its `base`, `typtypmod`, `typnotnull`, and CHECK constraints, an `enum` with its `pg_enum` labels, or a `composite`, `range`, `multirange`, or `pseudo` type |
| Type `comment` | `obj_description(oid, 'pg_type')`, for types created by users or extensions only; built-in types' descriptions are not read |

Expressions, view definitions, and display names are as PostgreSQL prints
them, which qualifies names off the `search_path`. Statistics, owners,
privileges, and row security policies are not read.

Table metadata derives from the document. A column is not nullable when its
own `NOT NULL` is validated or a domain its type resolves through is
`NOT NULL`; CHECK constraints are not interpreted.

For example, this table in schema `metadata_test`:

```sql
CREATE DOMAIN positive AS integer CHECK (VALUE > 0);
CREATE DOMAIN small_positive AS positive NOT NULL CONSTRAINT below_hundred CHECK (VALUE < 100);
COMMENT ON DOMAIN small_positive IS 'A positive number below 100';
CREATE TABLE filled (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  parent_id bigint CONSTRAINT filled_parent_fkey REFERENCES parent (id),
  quantity small_positive,
  amount integer CONSTRAINT amount_non_negative CHECK (amount >= 0),
  doubled integer GENERATED ALWAYS AS (amount * 2) STORED,
  created_at timestamptz DEFAULT now(),
  CONSTRAINT filled_parent_created_key UNIQUE (parent_id, created_at)
);
COMMENT ON TABLE filled IS 'Order lines';
COMMENT ON COLUMN filled.quantity IS 'Units ordered';
```

has this source metadata on PostgreSQL 18.6, read with the server's default
`search_path`; the `oid` differs per database:

```json
{
  "namespace": "metadata_test",
  "name": "filled",
  "oid": "25581",
  "kind": "table",
  "comment": "Order lines",
  "definition": null,
  "columns": [
    {
      "name": "id",
      "attnum": 1,
      "not_null": { "validated": true },
      "data_source_type": "bigint",
      "type": "bigint",
      "typmod": -1,
      "comment": null,
      "value_source": { "kind": "identity", "generation": "always" }
    },
    {
      "name": "parent_id",
      "attnum": 2,
      "not_null": null,
      "data_source_type": "bigint",
      "type": "bigint",
      "typmod": -1,
      "comment": null,
      "value_source": null
    },
    {
      "name": "quantity",
      "attnum": 3,
      "not_null": null,
      "data_source_type": "metadata_test.small_positive",
      "type": "metadata_test.small_positive",
      "typmod": -1,
      "comment": "Units ordered",
      "value_source": null
    },
    {
      "name": "amount",
      "attnum": 4,
      "not_null": null,
      "data_source_type": "integer",
      "type": "integer",
      "typmod": -1,
      "comment": null,
      "value_source": null
    },
    {
      "name": "doubled",
      "attnum": 5,
      "not_null": null,
      "data_source_type": "integer",
      "type": "integer",
      "typmod": -1,
      "comment": null,
      "value_source": { "kind": "generated", "storage": "stored", "expression": "(amount * 2)" }
    },
    {
      "name": "created_at",
      "attnum": 6,
      "not_null": null,
      "data_source_type": "timestamp with time zone",
      "type": "timestamp with time zone",
      "typmod": -1,
      "comment": null,
      "value_source": { "kind": "default", "expression": "now()" }
    }
  ],
  "types": [
    { "name": "bigint", "schema": "pg_catalog", "typname": "int8", "comment": null, "kind": "base" },
    { "name": "integer", "schema": "pg_catalog", "typname": "int4", "comment": null, "kind": "base" },
    {
      "name": "metadata_test.positive",
      "schema": "metadata_test",
      "typname": "positive",
      "comment": null,
      "kind": "domain",
      "base": "integer",
      "typmod": -1,
      "not_null": false,
      "checks": [{ "name": "positive_check", "expression": "(VALUE > 0)", "validated": true }]
    },
    {
      "name": "metadata_test.small_positive",
      "schema": "metadata_test",
      "typname": "small_positive",
      "comment": "A positive number below 100",
      "kind": "domain",
      "base": "metadata_test.positive",
      "typmod": -1,
      "not_null": true,
      "checks": [{ "name": "below_hundred", "expression": "((VALUE)::integer < 100)", "validated": true }]
    },
    { "name": "timestamp with time zone", "schema": "pg_catalog", "typname": "timestamptz", "comment": null, "kind": "base" }
  ],
  "primary_key": { "name": "filled_pkey", "columns": ["id"] },
  "unique_keys": [
    {
      "name": "filled_parent_created_key",
      "kind": "constraint",
      "keys": [{ "column": "parent_id" }, { "column": "created_at" }],
      "condition": null
    }
  ],
  "foreign_keys": [
    {
      "name": "filled_parent_fkey",
      "columns": ["parent_id"],
      "referenced": { "namespace": "metadata_test", "table": "parent", "columns": ["id"] },
      "enforced": true,
      "validated": true
    }
  ],
  "checks": [
    { "name": "amount_non_negative", "expression": "(amount >= 0)", "enforced": true, "validated": true }
  ]
}
```

The document's shape can change between connector versions. Source metadata
stored before such a change cannot be compared with newly retrieved source
metadata ([Comparing table metadata](../../../docs/connector-contract.md#comparing-table-metadata)).
A document whose type references do not resolve cannot be restored either,
so comparing it is also an invalid request.

## Type mapping

The Data Source type is the result of `format_type(atttypid, atttypmod)` over
`pg_catalog`, such as `character varying(10)` or `timestamp with time zone`,
matched as [Type mappings and coverage](../../../docs/connector-contract.md#type-mappings-and-coverage)
describes. The comparison column states how PostgreSQL compares the type
differently from the
[Analytics comparison](../../../docs/analytics-types.md#comparison), and the
last column which comparisons between two operands of the type run at
PostgreSQL ([Pushdown](#pushdown)):

| Data Source type | Analytics type | PostgreSQL's comparison | Runs remotely |
| --- | --- | --- | --- |
| `boolean` | `Boolean` | The same | Equality and ordering |
| `smallint`, `integer`, `bigint` | `SmallInt`, `Int`, `BigInt` | The same | Equality and ordering |
| `real`, `double precision` | `Float`, `Double` | The same: NaN equals NaN and orders after every other value, and -0 equals +0 | None |
| `numeric(p,s)` | `Decimal(p, s)` when p is at most 76; `numeric` without precision is unsupported | The same | Equality and ordering |
| `text`, `character varying(n)` | `Text` | Equality is the same under a deterministic collation, where equal strings are byte-equal; ordering follows the collation | Equality and inequality on a column with a deterministic collation, with a constant without NUL or with a column of the same collation |
| `character(n)` | `Text` | Ignores trailing spaces | None |
| `bytea` | `Blob` | The same | None |
| `date` | `Date` | The same | Equality and ordering, with a constant within `date`'s range, 4713 BC to 5874897 AD |
| `time without time zone` | `Time` | The same | Equality and ordering |
| `timestamp without time zone` | `Timestamp` | The same | Equality and ordering, with a constant from 4713 BC |
| `timestamp with time zone` | `TimestampTz` | The same | Equality and ordering, with a constant from 4713 BC |
| `interval` | `Interval` | Compares after normalizing 24 hours to a day and 30 days to a month, so 1 month equals 30 days, and orders intervals | None |
| `uuid` | `Uuid` | Compares values, accepting spellings other than the canonical one | Every comparison, with a constant in canonical form |
| `json`, `jsonb` | `Json` | `json` has no equality; `jsonb` compares parsed values | None |
| User-defined enum | `Enum` | The same equality; also orders members by declaration | None |
| `time with time zone` | `TimeWithTimeZone` | The same equality | None |
| Domain over one of the types above | The same as its base type; the Data Source type keeps the domain's name | As its base type | As its base type |

Arrays are not mapped. PostgreSQL's catalog does not enforce an array's number
of dimensions, so the nesting depth of `Array(T)` cannot be determined from the
schema.

Remote evaluation is narrower than agreement allows: floating-point and
`bytea` comparisons, and text ordering under the `C` collation when
`server_encoding` is `UTF8`, agree with the Analytics comparison but stay in
the local plan.

## Coverage

Version 0.1.0 covers every mapping rule in its [type mapping](#type-mapping). The
following values cannot be carried exactly and are `data` errors when a read
returns them:

- `numeric` `NaN` and infinities;
- `time` `24:00:00`;
- `date`, `timestamp`, and `timestamp with time zone` `infinity` and
  `-infinity`, and timestamps beyond the range of Arrow microsecond
  timestamps, such as `294276-12-31 23:59:59.999999`. The driver returns them
  as wrong values below PostgreSQL's earliest date, so the connector rejects
  any value before 4714-11-24 BC.

## Pushdown

| Offered work | Runs remotely when |
| --- | --- |
| Filter | It combines comparisons and null tests with AND, OR, and NOT, and every comparison is between columns and constants of one type and runs remotely by the [type mapping](#type-mapping). The whole filter moves, or none of it does. |
| Projection | It selects plain columns and no filter remains local below it |

## Known limitations

These come from the driver and are not yet handled by the connector:

- An interval whose time part does not fit in nanoseconds fails the read as a
  `data-source` error rather than a `data` error.
- A failed authentication is reported without a SQLSTATE, so it is classified
  as `connection` rather than `authentication`.

This comes from the Rust ADBC driver manager, which does not yet import an
error's status, SQLSTATE, and details from a result stream
([apache/arrow-adbc#4984](https://github.com/apache/arrow-adbc/issues/4984)):

- A connection the server terminates while a read is pulled fails the read as
  a `data-source` error, because only the error's message reaches the
  connector, rather than as a `connection` error. The session therefore stays
  usable until its next request fails.

The [conformance suite](#tests) checks each of these as a known deviation and
fails once the connector meets the contract.

## Compatibility

Only combinations that passed this crate's tests are listed.

| PostgreSQL | ADBC driver | Platform |
| --- | --- | --- |
| 18.6 | 1.13.0 | macOS arm64 |

## Tests

The integration tests start a PostgreSQL container with Docker and load the
driver from `ADBC_DRIVER_DIRECTORY`, which the Nix development shell sets.
`tests/conformance` runs the SDK's connector conformance suite with this
connector's fixture:

```sh
nix develop --command cargo test -p connector-postgresql
```

`POSTGRESQL_TEST_IMAGE` selects another server image, such as `postgres:14`.
The container is kept between runs and can be removed with `docker rm -f`.
