# SQL Server through ODBC connector

The SQL Server through ODBC connector reads SQL Server through the Microsoft
ODBC Driver 18 for SQL Server (direct access through `connector-odbc`). It
reads the `sys` catalog views itself and maps types by its
[type mapping](#type-mapping). It is a Generic ODBC Connector: its Data Source
types come from SQL Server's own catalog, and its values from the ODBC driver.

## Data Source access

| Item | Value |
| --- | --- |
| Reader crates | `odbc-api` 30.1.0 and `arrow-odbc` 27.0.0, through `connector-odbc` |
| Connection options | `server` (`host`, `host,port`, or `host\instance`), `database`, `user`, `password` (secret), `encrypt` (`yes`, `no`, or `strict`; default `yes`), and `trust_server_certificate` (`yes` or `no`; default `no`) |
| Fixed session settings | None |
| Fixed driver options | The driver `msodbcsql18/lib/msodbcsql18` in the library's ODBC driver directory and `ConnectRetryCount=0`, so that a broken connection is not reopened behind the session; text is fetched as UTF-16; each column is read as the [value conversion](#coverage) expects |

The connector authenticates with SQL Server authentication only. SQL Server
has no session time zone: `datetimeoffset` values carry their own offset,
and the other date and time types none.

## Dependencies

| Dependency | Used for | Ships with the connector | Tested versions |
| --- | --- | --- | --- |
| Microsoft ODBC Driver 18 for SQL Server (Microsoft license, not free) | Data Source access | Downloaded: the package's `bin/fetch-dependencies fetch msodbcsql18 --accept-license` fetches it from Microsoft into the ODBC driver directory, because its license does not allow us to redistribute it | 18.1.1.1 |
| ODBC driver manager: unixODBC (LGPL-2.1) | Loading the ODBC driver | Yes, on Linux: in the package's `lib/`, linked dynamically | 2.3.14 |
| MIT Kerberos (MIT) | Linked by the ODBC driver, which the connector authenticates without | Yes, on Linux: beside the driver | 1.22.2 |
| OpenSSL 3 (Apache-2.0) | The ODBC driver's encryption, loaded by the driver at run time, also without encryption | Yes, on Linux: beside the driver | 3.6.5 |

The tested versions are those of the development shell, which the
[conformance suite](#tests) runs with. The Linux packages pin their own:
driver 18.7.1.1, unixODBC 2.3.14, MIT Kerberos 1.22.3, and OpenSSL 3.5.9
(`packaging/dependencies.json`), with which the package check reads SQL
Server 2022 ([Packaging](../../../packaging/README.md)).

The connector is built into the Linux packages only. On macOS the driver
refers to its driver manager by an absolute Homebrew path, so it cannot be
placed in a package without rewriting it; there the development shell provides
the driver and its dependencies.

Exceptions: the ODBC driver manager is a native library the connector links
dynamically, which the guideline of keeping native dependencies in ADBC drivers
does not cover
([Reach ODBC sources through direct access](../../../docs/design/reach-odbc-sources-through-direct-access.md)).

The connector loads the ODBC driver from the library's ODBC driver directory
as `msodbcsql18/lib/msodbcsql18`, the driver's shared library or a link to it;
the driver manager's registered drivers are not used. The driver finds its
resources relative to that path, so the driver's `share` directory is
`msodbcsql18/share` beside it. On Linux the driver names Kerberos, OpenSSL,
and the driver manager's `libodbcinst` without a search path, so before the
driver loads, the connector loads `libkrb5.so.3`, `libgssapi_krb5.so.2`,
`libcrypto.so.3`, and `libssl.so.3` from `msodbcsql18/lib/` and
`libodbcinst.so.2` from beside the library, and the driver uses those.

## Source metadata

The source metadata of a table is the JSON document of `SqlServerTable`. It
holds what the `sys` catalog views state about the table or view: the facts
its table metadata derives from, and what states its meaning for the Semantic
Catalog, such as comments, view definitions, and column expressions. The
rustdoc of `SqlServerTable` and the types it contains defines each field.
The static information publishes the document's JSON Schema, derived from
these types, with that rustdoc as its descriptions.

| Item | From |
| --- | --- |
| Table: `namespace`, `name`, `object_id`, `kind` | `sys.objects` (`U` user tables and `V` views that do not come with SQL Server), `sys.schemas` |
| Table: `definition_visible` | `HAS_PERMS_BY_NAME(..., 'OBJECT', 'VIEW DEFINITION')` |
| Table: `comment`, column `comment` | The `MS_Description` extended property |
| Table: `definition`, for views | `sys.sql_modules.definition`; `null` for an encrypted view |
| Column: `name`, `column_id`, `nullable`, `max_length`, `precision`, `scale` | `sys.columns` |
| Column: `type` | `sys.types`: its `name`, `schema`, `user_defined`, and the `system_type` an alias type is created from; `null` for a CLR type |
| Column: `value_source` | An identity column (`is_identity`), a computed column with `persisted` and its expression (`sys.computed_columns`), or a default with its expression (`sys.default_constraints`) |
| `primary_key`, `unique_keys` | `sys.indexes` with `is_unique`, its key columns in `sys.index_columns`, `is_disabled` as `enforced`, and a filtered index's `filter_definition` as `condition` |
| `foreign_keys` | `sys.foreign_keys` and `sys.foreign_key_columns`, `is_disabled` and `is_not_trusted` as `enforced` and `validated` |
| `checks` | `sys.check_constraints` with its `definition`, `is_disabled`, and `is_not_trusted` |

Expressions and definitions are as SQL Server stores them, such as
`([amount]*(2))`. They have no length limit: the connector reads each in
pieces of 8000 bytes of its UTF-16 text and joins them. Statistics, owners,
permissions, and non-unique indexes are not read.

Table metadata derives from the document, and no connection option changes
the derivation. A column's ordinal is its position;
its source identifier is its `column_id`. Constraints are those of the
document. They are missing when the credential lacks `VIEW DEFINITION` on the
table, which hides them, and when a filtered unique index's condition, a
CHECK expression, or a foreign key's referenced table is not visible.

SQL Server hides the tables a credential has no permission on. A requested
table that is not found therefore does not exist only when the credential has
`VIEW DEFINITION` on the database and none of its principals is denied
`VIEW DEFINITION` or `CONTROL` there; otherwise it is "not found". Lists hold
only the schemas and tables the credential can see.

For example, this table in schema `sales`:

```sql
CREATE TABLE sales.orders (
  id int IDENTITY(1, 1) NOT NULL CONSTRAINT orders_pkey PRIMARY KEY,
  amount decimal(10, 2) NOT NULL CONSTRAINT orders_amount_default DEFAULT (0)
    CONSTRAINT orders_amount_check CHECK (amount >= 0),
  doubled AS (amount * 2) PERSISTED
);
EXEC sp_addextendedproperty N'MS_Description', N'Orders',
  N'SCHEMA', N'sales', N'TABLE', N'orders';
```

has this source metadata on SQL Server 2022; the `object_id` differs per
database:

```json
{
  "namespace": "sales",
  "name": "orders",
  "object_id": 901578250,
  "kind": "table",
  "definition_visible": true,
  "comment": "Orders",
  "definition": null,
  "columns": [
    {
      "name": "id",
      "column_id": 1,
      "nullable": false,
      "type": { "name": "int", "schema": "sys", "user_defined": false, "system_type": "int" },
      "max_length": 4,
      "precision": 10,
      "scale": 0,
      "comment": null,
      "value_source": { "kind": "identity" }
    },
    {
      "name": "amount",
      "column_id": 2,
      "nullable": false,
      "type": { "name": "decimal", "schema": "sys", "user_defined": false, "system_type": "decimal" },
      "max_length": 9,
      "precision": 10,
      "scale": 2,
      "comment": null,
      "value_source": { "kind": "default", "expression": "((0))" }
    },
    {
      "name": "doubled",
      "column_id": 3,
      "nullable": true,
      "type": { "name": "decimal", "schema": "sys", "user_defined": false, "system_type": "decimal" },
      "max_length": 9,
      "precision": 12,
      "scale": 2,
      "comment": null,
      "value_source": { "kind": "computed", "persisted": true, "expression": "([amount]*(2))" }
    }
  ],
  "primary_key": { "name": "orders_pkey", "columns": ["id"], "enforced": true },
  "unique_keys": [],
  "foreign_keys": [],
  "checks": [
    { "name": "orders_amount_check", "expression": "([amount]>=(0))", "enforced": true, "validated": true }
  ]
}
```

The document's shape can change between connector versions. Source metadata
stored before such a change cannot be compared with newly retrieved source
metadata ([Comparing table metadata](../../../docs/connector-contract.md#comparing-table-metadata)).

## Type mapping

The Data Source type is read from `sys.columns` and `sys.types`. A system type
is spelled with its parameters, such as `nvarchar(50)`, `varbinary(max)`,
`decimal(10,2)`, or `datetime2(7)`, the length of `nchar` and `nvarchar` in
characters. An alias type is spelled by its name, qualified with its schema
when a user created it, such as `sales.code` or `sysname`, and maps as the
system type it is created from. The comparison column states how SQL Server
compares the type differently from the
[Analytics comparison](../../../docs/analytics-types.md#comparison), and the
last column which comparisons between two operands of the type run at SQL
Server ([Pushdown](#pushdown)):

| Data Source type | Analytics type | SQL Server's comparison | Runs remotely |
| --- | --- | --- | --- |
| `bit` | `Boolean` | The same | Equality and ordering |
| `tinyint` | `UnsignedByte` | The same | Equality and ordering |
| `smallint`, `int`, `bigint` | `SmallInt`, `Int`, `BigInt` | The same | Equality and ordering |
| `real`, `float` | `Float`, `Double` | The same; SQL Server holds no NaN or infinities | None |
| `decimal(p,s)`, `numeric(p,s)` | `Decimal(p, s)` | The same | Equality and ordering, with a constant of at most 38 digits and no negative scale |
| `money`, `smallmoney` | `Decimal(19, 4)`, `Decimal(10, 4)` | The same | Equality and ordering |
| `char(n)`, `varchar(n)`, `nchar(n)`, `nvarchar(n)` | `Text` | Follows the collation, which may ignore case and accents, and ignores trailing spaces | None |
| `varchar(max)`, `nvarchar(max)`, `text`, `ntext`, `xml` | `Text` | As above; `xml` has no comparison | None |
| `binary(n)`, `varbinary(n)`, `varbinary(max)`, `image` | `Blob` | Ignores trailing zero bytes | None |
| `timestamp` (`rowversion`) | `Blob` | The same | None |
| `date` | `Date` | The same | Equality and ordering, with a constant from 0001-01-01 to 9999-12-31 |
| `time(p)` | `Time` | The same | Equality and ordering |
| `datetime2(p)` | `Timestamp` | The same | Equality and ordering, with a constant from 0001-01-01 to 9999-12-31 |
| `datetime`, `smalldatetime` | `Timestamp` | Compared after conversion to `datetime2` in their exact 1/300-second or one-minute ticks, while `datetime` values are read rounded to the millisecond ([Known limitations](#known-limitations)) | None |
| `datetimeoffset(p)` | `TimestampTz`, the instant; the offset is not carried | The same: by instant | None |
| `uniqueidentifier` | `Uuid` | The same equality; ordering by SQL Server's own byte order | Equality and inequality, with a constant in canonical form |

`sql_variant`, `hierarchyid`, `geometry`, `geography`, other CLR types, and
SQL Server 2025's `json` and `vector` are not mapped.

Remote evaluation is narrower than agreement allows: floating-point,
`timestamp`, and `datetimeoffset` comparisons agree with the Analytics
comparison but stay in the local plan.

## Coverage

Version 0.1.0 covers every mapping rule in its [type mapping](#type-mapping)
except those of `varchar(max)`, `nvarchar(max)`, `text`, `ntext`, `xml`,
`varbinary(max)`, and `image`: `arrow-odbc` reads each column into a buffer of
a fixed length, so it cannot read values of unbounded length whole. Their
columns are unsupported with the reason "mapping not implemented".

`arrow-odbc` reads most covered types in their Analytics storage. The others
are read as follows and converted without changing any value:

| Mapping rule | Read as | Conversion |
| --- | --- | --- |
| `decimal`, `numeric`, `money`, `smallmoney` | Decimal text | To the narrowest Arrow decimal of the precision |
| `binary(n)`, `timestamp` | Binary | None |
| `datetime`, `smalldatetime`, `datetime2(p)` up to 6 digits | Timestamps in seconds, milliseconds, or microseconds | To microseconds |
| `datetime2(7)` | Text | To microseconds |
| `time(p)` | Text | To microseconds |
| `datetimeoffset(p)` | Text | To the UTC instant in microseconds |
| `uniqueidentifier` | Uppercase text | To lowercase canonical text |

The following values cannot be carried exactly and are `data` errors when a
read returns them:

- `time`, `datetime2`, and `datetimeoffset` values whose seventh fractional
  digit, a fraction of a microsecond, is not zero.

A column whose alias type the credential has no permission on has no Data
Source type, because SQL Server hides the type; the table metadata is then
incomplete.

## Pushdown

| Offered work | Runs remotely when |
| --- | --- |
| Filter | It combines comparisons and null tests with AND, OR, and NOT, and every comparison is between columns and constants of one type and runs remotely by the [type mapping](#type-mapping). A Boolean column or constant is never a condition by itself, because SQL Server has no Boolean expressions. The whole filter moves, or none of it does. |
| Projection | It selects plain columns and no filter remains local below it |

## Known limitations

These come from the ODBC driver and `arrow-odbc`, and are not yet handled by
the connector:

- A `datetime` value is a whole number of 1/300-second ticks, so most values
  ending in .003 or .007 seconds have no exact microsecond value and should be
  `data` errors. The ODBC driver returns them rounded to the millisecond, as
  `datetime2(3)` would be, and the connector returns that rounded value. ODBC
  describes both types alike, so the read cannot tell them apart.
- An `nchar` or `nvarchar` value holding an unpaired surrogate, which
  non-`_SC` collations allow, should be a `data` error. `arrow-odbc` converts
  the UTF-16 text to UTF-8 with U+FFFD in its place.

The [conformance suite](#tests) checks the first as a known deviation and
fails once the connector meets the contract.

In the Linux packages, the OpenSSL the package carries looks for the trusted
certificates under `/etc/ssl`, as it was built. Where the system keeps them
elsewhere, as Red Hat Enterprise Linux keeps them under `/etc/pki`, verifying
the server's certificate (`trust_server_certificate` `no`, the default) may
fail. Certificate verification with the packaged OpenSSL has not been tested.

## Compatibility

Only combinations that passed this crate's tests are listed.

| SQL Server | ODBC driver | ODBC driver manager | Platform |
| --- | --- | --- | --- |
| 2022 (`mcr.microsoft.com/mssql/server:2022-latest`, `linux/amd64`) | 18.1.1.1 | unixODBC 2.3.14 | macOS arm64 |

## Tests

The integration tests start a SQL Server container with Docker as
`linux/amd64`, which runs under emulation on arm64 hosts, and reach it through
the ODBC driver in `ODBC_DRIVER_DIRECTORY`, which the Nix development shell
sets.
`tests/conformance` runs the SDK's connector conformance suite with this
connector's fixture:

```sh
nix develop --command cargo test -p connector-sqlserver-odbc
```

`SQLSERVER_TEST_IMAGE` selects another server image. The container is kept
between runs and can be removed with `docker rm -f`.

`tests/package/e2e.sh` checks the connector in a Linux distribution package,
run by the package check (`packaging/test/check-package.sh`) after the driver
was fetched: the packaged DuckDB extension reads SQL Server with and without
encryption, and the driver uses the package's ODBC driver manager, Kerberos,
and OpenSSL, whatever the system has. It uses the same container, or the
server that `SQLSERVER_TEST_PORT` names, set up with `tests/package/setup.sql`,
as `packaging/linux/check.sh` provides. Without either, an arm64 host skips
it, since the server image is `linux/amd64` only and the package check runs
without emulation; CI runs it on linux_amd64.
