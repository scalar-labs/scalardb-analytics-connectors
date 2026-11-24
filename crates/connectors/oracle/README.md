# Oracle connector

The Oracle connector reads Oracle Database through the Oracle Instant Client
ODBC driver (direct access through `connector-odbc`). It reads the `ALL_*`
data dictionary views itself and maps types by its
[type mapping](#type-mapping): its Data Source types come from Oracle's own
catalog, and its values from the ODBC driver.

## Data Source access

| Item | Value |
| --- | --- |
| Reader crates | `odbc-api` 30.1.0 and `arrow-odbc` 27.0.0, through `connector-odbc` |
| Connection options | `host`, `port` (default `1521`), `service_name`, `user`, and `password` (secret) |
| Fixed session settings | `TIME_ZONE` to `+00:00`, `NLS_NUMERIC_CHARACTERS` to `.,`, and `NLS_COMP` and `NLS_SORT` to `BINARY`, run right after connecting |
| Fixed driver options | The driver `instantclient/libsqora` in the library's ODBC driver directory and `FEN=F`, so that the driver's failover does not reopen a broken connection behind the session; statements are passed and text is fetched as UTF-16; each column is read as the [value conversion](#coverage) expects |

The connector connects to the database `//host:port/service_name`, Oracle's
Easy Connect form, and authenticates with a database user and password. Easy
Connect takes an IPv6 address only in brackets, so a `host` containing `:`,
such as `::1`, is written as `[::1]`; one given in brackets is written as
given. The
driver keeps braces as part of a connection string value, so a value with a
character the connection string grammar gives meaning, such as `=` in a
password, is written in double quotes. The driver ends a value at `;` however
it is written, so an option containing `;` is `invalid settings`.

The driver converts `TIMESTAMP WITH TIME ZONE` and `TIMESTAMP WITH LOCAL TIME
ZONE` values to the session time zone and drops their offset, so the fixed
UTC time zone makes them arrive as their UTC instant. `NUMBER` values are read
as text, so the fixed numeric characters give them a `.` decimal separator.
The binary comparison and sort make name filters match exactly and order
names by code point.

## Dependencies

| Dependency | Used for | Ships with the connector | Tested versions |
| --- | --- | --- | --- |
| Oracle Instant Client Basic and ODBC packages (Oracle Technology Network License, not free) | Data Source access | No: installed by the user in the ODBC driver directory, because its license does not allow us to redistribute it | 23.3.0.23.09 and 23.26.4.0.0 |
| ODBC driver manager: unixODBC on Linux and macOS (LGPL-2.1) | Loading the ODBC driver; the ODBC driver also links its installer library, `libodbcinst` | No: installed by the user, and linked dynamically | 2.3.14 |

The connector is not built into the distribution packages yet
([Packaging](../../../packaging/README.md)).

Exceptions: the ODBC driver manager is a native library the connector links
dynamically, which the guideline of keeping native dependencies in ADBC drivers
does not cover
([Reach ODBC sources through direct access](../../../docs/design/reach-odbc-sources-through-direct-access.md)).

The connector loads the ODBC driver from the library's ODBC driver directory
as `instantclient/libsqora`, the driver's shared library or a link to it
(`libsqora.so.23.1` on Linux and `libsqora.dylib.23.1` on macOS); the driver
manager's registered drivers are not used. The driver loads the Instant Client
Basic libraries, such as `libclntsh`, from its own directory, so both packages
are unpacked into `instantclient`. The driver finds unixODBC's
`libodbcinst` on the library search path.

The connector requires Instant Client 23 or later. Earlier ODBC drivers, such
as 21.10, describe a `BOOLEAN` column as a floating-point number and return 1.0
and 0.0, which the connector does not read as Boolean.

## Source metadata

The source metadata of a table is the JSON document of `OracleTable`. It holds
what the `ALL_*` data dictionary views state about the table or view: the
facts its table metadata derives from, and what states its meaning for the
Semantic Catalog, such as comments, view definitions, and column expressions.
The rustdoc of `OracleTable` and the types it contains defines each field. The
static information publishes the document's JSON Schema, derived from these
types, with that rustdoc as its descriptions.

| Item | From |
| --- | --- |
| Table: `namespace`, `name`, `kind` | `ALL_TABLES`, without nested table storage, domain index data, index-organized table overflow segments, and dropped tables; and `ALL_VIEWS` |
| Table: `object_id` | `ALL_OBJECTS` |
| Table: `comment`, column `comment` | `ALL_TAB_COMMENTS`, `ALL_COL_COMMENTS` |
| Table: `definition`, for views | `ALL_VIEWS.TEXT` |
| Column: `name`, `column_id`, `nullable`, `data_type`, `data_type_owner`, `data_length`, `data_precision`, `data_scale`, `char_length`, `char_used` | `ALL_TAB_COLS`, without hidden columns |
| Column: `value_source` | An identity column (`IDENTITY_COLUMN`), a virtual column with its expression (`VIRTUAL_COLUMN` and `DATA_DEFAULT`), or a default with its expression and `DEFAULT_ON_NULL` (`DATA_DEFAULT`) |
| `primary_key`, `unique_keys`, `foreign_keys`, `checks` | `ALL_CONSTRAINTS` of type `P`, `U`, `R`, and `C` that involve no hidden column, their columns in `ALL_CONS_COLUMNS`, `STATUS` as `enforced`, `VALIDATED` as `validated`, and a CHECK's `SEARCH_CONDITION` |
| `unique_keys` of kind `index` | `ALL_INDEXES` that are unique, on columns the table's columns list (virtual columns among them), and back no primary key or unique constraint, with their columns in `ALL_IND_COLUMNS` |

Definitions and expressions are as Oracle stores them, such as `"AMOUNT"*2`.
Oracle keeps them as `LONG` values, which `arrow-odbc` cannot read, and cuts
the `VARCHAR2` copies beside them at 4000 bytes, so the connector reads each in
an inline PL/SQL function as UTF-8 bytes in pieces of 2000 bytes and joins
them. Each is `{"text": ...}`, or the reason it is not read: `"too_long"`
when it is longer than the connector reads (see
[Known limitations](#known-limitations)), or `"not_utf8"` when its UTF-8
conversion is not valid UTF-8, which leaves only that item unread. A value
longer than the connector reads is never read, so it is `"too_long"` even
when it is not valid UTF-8. Oracle documents a replacement character in
`CONVERT` only for a character the destination character set lacks, and
`AL32UTF8` represents every character of the database character set, so a
value is `"not_utf8"` only when its stored bytes are not valid in the database
character set and pass through the conversion. Oracle does not document what
`CONVERT` does with such bytes, and this has not been tested on a database
whose character set is not `AL32UTF8`. A NOT NULL constraint, which Oracle stores as the CHECK condition
`"<column>" IS NOT NULL` on its one column, is the column's
nullability and is not among `checks` when the column is not nullable. Oracle
marks a column not nullable only for an enabled and validated NOT NULL
constraint, so one that is disabled or not validated is among `checks`. The
condition cannot tell a NOT NULL constraint from a CHECK constraint written
as `CHECK ("X" IS NOT NULL)`, so such a CHECK constraint is among `checks`
only when its column is nullable; on a column that is not nullable it is
redundant and not read. A constraint that involves
a hidden column, such as an invisible one, is not read, because the columns do
not list that column: a key or CHECK constraint with any of its columns
hidden, the NOT NULL constraint of a hidden column, and a foreign key that
references a key on a hidden column. Statistics, storage,
partitions, privileges, triggers, non-unique indexes, and unique indexes on
expressions, including descending ones, are not read.

Table metadata derives from the document, and no connection option changes
the derivation. A column's ordinal is its `COLUMN_ID`. A column has no source
identifier: Oracle renumbers the columns after a dropped one, so no column
number identifies a column across changes; the table's is its `object_id`.
Constraints are those of the document. They are missing when a foreign key's
referenced table is not visible or a CHECK expression is not read, and the
reason says whether it is too long or not valid UTF-8.

The `ALL_*` views show only the tables the credential has a privilege on,
such as `SELECT`, `READ`, or `INSERT`, or a system privilege such as `SELECT
ANY TABLE` grants. A requested table that is not found is therefore "not
found". A table the credential sees shows every column. Lists hold every
schema of a user Oracle does not maintain (`ORACLE_MAINTAINED` is `N`), and
only the tables and views the credential can see.

For example, this table in schema `SALES`:

```sql
CREATE TABLE sales.orders (
  id NUMBER(10) GENERATED ALWAYS AS IDENTITY CONSTRAINT orders_pkey PRIMARY KEY,
  amount NUMBER(10, 2) DEFAULT 0 NOT NULL CONSTRAINT orders_amount_check CHECK (amount >= 0),
  doubled NUMBER(12, 2) GENERATED ALWAYS AS (amount * 2) VIRTUAL
);
COMMENT ON TABLE sales.orders IS 'Orders';
COMMENT ON COLUMN sales.orders.amount IS 'Order total';
```

has this source metadata on Oracle Database 23ai Free 23.26.3; the
`object_id` differs per database, and the default keeps the space after `0`
as Oracle stores it:

```json
{
  "namespace": "SALES",
  "name": "ORDERS",
  "object_id": 73427,
  "kind": "table",
  "comment": "Orders",
  "definition": null,
  "columns": [
    {
      "name": "ID",
      "column_id": 1,
      "nullable": false,
      "data_type": "NUMBER",
      "data_type_owner": null,
      "data_length": 22,
      "data_precision": 10,
      "data_scale": 0,
      "char_length": 0,
      "char_used": null,
      "comment": null,
      "value_source": { "kind": "identity" }
    },
    {
      "name": "AMOUNT",
      "column_id": 2,
      "nullable": false,
      "data_type": "NUMBER",
      "data_type_owner": null,
      "data_length": 22,
      "data_precision": 10,
      "data_scale": 2,
      "char_length": 0,
      "char_used": null,
      "comment": "Order total",
      "value_source": { "kind": "default", "expression": { "text": "0 " }, "on_null": false }
    },
    {
      "name": "DOUBLED",
      "column_id": 3,
      "nullable": true,
      "data_type": "NUMBER",
      "data_type_owner": null,
      "data_length": 22,
      "data_precision": 12,
      "data_scale": 2,
      "char_length": 0,
      "char_used": null,
      "comment": null,
      "value_source": { "kind": "virtual", "expression": { "text": "\"AMOUNT\"*2" } }
    }
  ],
  "primary_key": { "name": "ORDERS_PKEY", "columns": ["ID"], "enforced": true, "validated": true },
  "unique_keys": [],
  "foreign_keys": [],
  "checks": [
    {
      "name": "ORDERS_AMOUNT_CHECK",
      "expression": { "text": "amount >= 0" },
      "enforced": true,
      "validated": true
    }
  ]
}
```

The document's shape can change between connector versions. Source metadata
stored before such a change cannot be compared with newly retrieved source
metadata ([Comparing table metadata](../../../docs/connector-contract.md#comparing-table-metadata)).

## Type mapping

The Data Source type is read from `ALL_TAB_COLS`. A built-in type is spelled
with its parameters, such as `NUMBER(10,2)`, `NUMBER(*,0)` for a `NUMBER`
with a scale and no precision, `VARCHAR2(20 CHAR)`, `CHAR(5 BYTE)`,
`NVARCHAR2(20)`, `RAW(16)`, `FLOAT(126)`, or `TIMESTAMP(6) WITH TIME ZONE`,
and an object type by its name qualified with its owner, such as
`MDSYS.SDO_GEOMETRY`. The comparison column states how Oracle compares the
type differently from the
[Analytics comparison](../../../docs/analytics-types.md#comparison), and the
last column which comparisons between two operands of the type run at Oracle
([Pushdown](#pushdown)):

| Data Source type | Analytics type | Oracle's comparison | Runs remotely |
| --- | --- | --- | --- |
| `NUMBER(p,s)`, 0 ≤ s ≤ p | `Decimal(p, s)` | The same | Equality and ordering |
| `NUMBER` without a precision, with a negative scale, or with a scale greater than the precision | Unsupported | — | — |
| `FLOAT(b)` | Unsupported: a decimal number of up to 126 binary digits, which no Analytics type carries exactly | — | — |
| `BINARY_FLOAT`, `BINARY_DOUBLE` | `Float`, `Double` | The same: NaN equals NaN and orders after positive infinity | None |
| `BOOLEAN` | `Boolean` | The same | None |
| `CHAR(n)`, `NCHAR(n)` | `Text` | Blank-padded: trailing spaces are ignored; follows the collation and `NLS_COMP` | None |
| `VARCHAR2(n)`, `NVARCHAR2(n)` | `Text` | Follows the collation and `NLS_COMP`, which may ignore case and accents | None |
| `CLOB`, `NCLOB` | `Text` | Not comparable | None |
| `RAW(n)` | `Blob` | The same | None |
| `BLOB` | `Blob` | Not comparable | None |
| `DATE` | `Timestamp`, to the second | The same | Equality and ordering, with a constant from 0001-01-01 to 9999-12-31, other than 1582-10-05 to 1582-10-14 |
| `TIMESTAMP(n)`, n ≤ 6 | `Timestamp` | The same | Equality and ordering, with a constant from 0001-01-01 to 9999-12-31, other than 1582-10-05 to 1582-10-14 |
| `TIMESTAMP(n) WITH TIME ZONE`, n ≤ 6 | `TimestampTz`, the instant; the offset or region is not carried | The same: by instant | Equality and ordering, with a constant from 0001-01-01 to 9999-12-31, other than 1582-10-05 to 1582-10-14 |
| `TIMESTAMP(n) WITH LOCAL TIME ZONE`, n ≤ 6 | `TimestampTz` | The same: by instant | Equality and ordering, with a constant from 0001-01-01 to 9999-12-31, other than 1582-10-05 to 1582-10-14 |
| `TIMESTAMP(n)` and its time zone forms, n from 7 to 9 | Unsupported: a fraction of a microsecond, which no Analytics type carries | — | — |

`LONG`, `LONG RAW`, `ROWID`, `UROWID`, `BFILE`, `INTERVAL YEAR TO MONTH`,
`INTERVAL DAY TO SECOND`, `JSON`, `VECTOR`, `XMLTYPE`, spatial types, and other
object types are not mapped. A column with a domain maps as its built-in type.

Oracle reads an empty `VARCHAR2` or `RAW` value as NULL, so it holds no empty
`Text` or `Blob` value.

Remote evaluation is narrower than agreement allows: floating-point, `RAW`,
and `BOOLEAN` comparisons agree with the Analytics comparison but stay in the
local plan.

## Coverage

Version 0.1.0 covers every mapping rule in its [type mapping](#type-mapping)
except those of `CLOB`, `NCLOB`, and `BLOB`: `arrow-odbc` reads each column
into a buffer of a fixed length, so it cannot read values of unbounded length
whole. Their columns are unsupported with the reason "mapping not
implemented".

`arrow-odbc` reads most covered types in their Analytics storage. The others
are read as follows and converted without changing any value:

| Mapping rule | Read as | Conversion |
| --- | --- | --- |
| `NUMBER(p,s)` | Decimal text, such as `-.5` | To the narrowest Arrow decimal of the precision |
| `DATE`, `TIMESTAMP(n)` | Microsecond timestamps; the driver describes every date and timestamp without fractional digits, which `arrow-odbc` would read in seconds | None |
| `TIMESTAMP(n) WITH TIME ZONE`, `WITH LOCAL TIME ZONE` | Microsecond timestamps in the UTC session time zone | Marked as UTC |
| `BOOLEAN` | 8-bit integers, 1 and 0; the driver does not return it as an ODBC bit | To Boolean |

No value of a covered rule fails to be carried exactly, except the Julian leap
days of [Known limitations](#known-limitations): every `NUMBER(p,s)` value fits
`Decimal(p, s)`, and a `TIMESTAMP(n)` with n ≤ 6 has no fraction of a
microsecond.

## Pushdown

| Offered work | Runs remotely when |
| --- | --- |
| Filter | It combines comparisons and null tests with AND, OR, and NOT, and every comparison is between columns and constants of one type and runs remotely by the [type mapping](#type-mapping). A Boolean column or constant is never a condition by itself. The whole filter moves, or none of it does. |
| Projection | It selects plain columns and no filter remains local below it |

Planning reads nothing from Oracle: whether a comparison runs remotely
depends only on the Analytics types of the offered plan, because every Oracle
type the connector maps to `Decimal`, `Timestamp`, or `TimestampTz` compares
as that type does.

## Known limitations

These come from Oracle's calendar, and are not yet handled by the connector:

- Oracle dates before 1582-10-15 are in the Julian calendar, and Oracle counts
  years before 1 without a year 0. The ODBC driver returns a date's year,
  month, day, and time, and the connector returns them as the proleptic
  Gregorian date and time with the same fields, so -4712-01-01 is returned as
  the Arrow timestamp of -4712-01-01. Comparisons at Oracle use the same
  fields, so a remote filter selects the rows the returned values select.
  Oracle reads a constant on a day its calendar skips, 1582-10-05 to
  1582-10-14, as 1582-10-15, so a comparison with one stays local.
- A Julian leap day that the proleptic Gregorian calendar does not have, such
  as 1500-02-29, has no timestamp with the same fields: `arrow-odbc` panics
  converting it, and the read fails with an `internal` error.

A view definition, CHECK condition, or column expression is read only up to
the length a PL/SQL value holds: 32,760 bytes in the database character set,
and 32,767 bytes once converted to UTF-8. In an `AL32UTF8` database, the
default, that is 32,760 bytes of UTF-8; in a database with another character
set, a value within 32,760 bytes can still exceed 32,767 bytes in UTF-8. A
longer value is `"too_long"` in the source metadata, and a longer CHECK
condition leaves the table's constraints missing. Reading longer values would
take reading the `LONG` columns piecewise with `SQLGetData`, outside the
Arrow reads the catalog uses.

Metadata requests cannot be made to wait at Oracle: the data dictionary views
are read consistently without locks, so the conformance suite does not check
stopping a waiting metadata request.

## Compatibility

Only combinations that passed this crate's tests are listed.

| Oracle Database | ODBC driver | ODBC driver manager | Platform |
| --- | --- | --- | --- |
| 23ai Free 23.26.3 (`gvenzl/oracle-free:23-slim-faststart`) | Instant Client 23.3.0.23.09 | unixODBC 2.3.14 | macOS arm64 |
| 23ai Free 23.26.3 (`gvenzl/oracle-free:23-slim-faststart`) | Instant Client 23.26.4.0.0 | unixODBC 2.3.14 | Linux x86_64 |

## Tests

The integration tests start an Oracle Database container with Docker and
reach it through the ODBC driver in `ODBC_DRIVER_DIRECTORY`, which the Nix
development shell sets. `tests/conformance` runs the SDK's connector
conformance suite with this connector's fixture:

```sh
nix develop --command cargo test -p connector-oracle
```

`ORACLE_TEST_IMAGE` selects another server image, which must serve the
pluggable database `FREEPDB1` and take the `SYSTEM` password from
`ORACLE_PASSWORD`, as the `gvenzl/oracle-free` images do. The container is
kept between runs and can be removed with `docker rm -f`.
