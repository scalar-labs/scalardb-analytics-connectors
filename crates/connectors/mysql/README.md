# MySQL connector

The MySQL connector reads MySQL through the ADBC Driver Foundry MySQL driver
(ADBC access). It reads `information_schema` itself and maps types by its
[type mapping](#type-mapping). The general conventions it follows are in
[Initial PostgreSQL and MySQL conventions](../../../docs/connector-implementation.md#initial-postgresql-and-mysql-conventions).

## Data Source access

| Item | Value |
| --- | --- |
| ADBC driver | `adbc_driver_mysql`, the ADBC Driver Foundry Driver for MySQL, loaded through its driver manifest `adbc_driver_mysql.toml` in the library's ADBC driver directory |
| Supported driver versions | `>=0.8.0, <0.9.0` |
| Connection options | `host`, `port`, `user`, `password` (secret), `tls`, and `tinyint1_as_boolean` |
| Fixed session settings | `time_zone='+00:00'`, the character set `utf8mb4`, and `sql_mode='NO_BACKSLASH_ESCAPES'` |
| Fixed driver options | `mysql.query.zero_datetime_behavior=error` |

The driver reports its version as `v0.8.0`. It fixes `time_zone` itself, so
`TIMESTAMP` values arrive as UTC instants. The connector passes its other
session settings as DSN parameters, which the driver runs as `SET`
statements after connecting. `sql_mode` keeps backslashes in generated
string literals ordinary characters; the server's other modes do not change
what a `SELECT` returns. Without the fixed driver option, the driver would
read a zero date as NULL.

`tls` takes the driver's modes: `true` verifies the server's certificate,
`skip-verify` encrypts without verifying it, `preferred` encrypts when the
server offers TLS, and `false` does not encrypt. The connection names no
default database: generated SQL qualifies every table.

`tinyint1_as_boolean`, `false` by default, chooses how `tinyint(1)` maps
([Type mapping](#type-mapping)). It is a setting of the Data Source rather
than of a session. It changes the table metadata derived from a table, not
the table's source metadata, which holds only what was read: switching it
makes the table metadata of the same table differ while its source metadata
does not.

## Dependencies

| Dependency | Used for | Ships with the connector | Tested versions |
| --- | --- | --- | --- |
| ADBC Driver Foundry Driver for MySQL (Apache-2.0) | Data Source access | Yes | 0.8.0 |

The driver is one self-contained shared library, built from Go with
`go-sql-driver/mysql` and Go's own TLS; its bundled license file lists the
licenses of what it contains, all permissive. No build is published for
macOS x86_64. Exceptions: none. No dependency is installed by the user.

## Source metadata

The source metadata of a table is the JSON document of `MysqlTable`. It
holds what `information_schema` shows the reading account about the table:
the facts its table metadata derives from, and what states the table's
meaning for the Semantic Catalog, such as comments, view definitions, and
how columns are filled. The rustdoc of `MysqlTable` and the types it
contains defines each field.

A document describes one table, so its top level is the table. Each fact
appears once, with the table or column it belongs to.

| Item | From |
| --- | --- |
| Table: `schema`, `name`, `kind`, `comment` | `TABLES`: `BASE TABLE` or `VIEW`, and `TABLE_COMMENT` for a base table |
| Table: `definition`, for views | `VIEWS.VIEW_DEFINITION`, which is empty, and recorded as `null`, when the account may not see it |
| Table: `columns_may_be_hidden` | The account's `SELECT` grants in `USER_PRIVILEGES`, `SCHEMA_PRIVILEGES`, and `TABLE_PRIVILEGES` |
| Table: `primary_key`, `unique_keys` | `STATISTICS` rows with `NON_UNIQUE = 0`: key columns, column prefix lengths (`SUB_PART`), and functional key parts (`EXPRESSION`) |
| Table: `foreign_keys` | `KEY_COLUMN_USAGE` rows with a referenced table |
| Table: `checks` | `CHECK_CONSTRAINTS.CHECK_CLAUSE` with `TABLE_CONSTRAINTS.ENFORCED` |
| Column: `name`, `ordinal_position`, `nullable` | `COLUMNS` |
| Column: `data_source_type`, `data_type` | `COLUMN_TYPE` and `DATA_TYPE` |
| Column: `character_set`, `collation`, `comment` | `CHARACTER_SET_NAME`, `COLLATION_NAME`, and `COLUMN_COMMENT` |
| Column: `value_source` | `COLUMN_DEFAULT`, `EXTRA`, and `GENERATION_EXPRESSION`: a literal default, a default expression, `AUTO_INCREMENT`, or a stored or virtual generated column |

Expressions and view definitions are as MySQL prints them. Statistics,
engines, partitions, triggers, `ON UPDATE` clauses, foreign key actions, and
privileges other than `SELECT` grants are not read.

Table metadata derives from the document and the Data Source's
`tinyint1_as_boolean` option. MySQL offers no identifiers for
tables or columns besides their names, so source identifiers are not
offered. A unique index with a functional key part is not a column
constraint and stays in the source metadata; a unique column prefix makes
its column unique. MySQL implements every unique constraint as a unique
index and reports each as a constraint.

`information_schema` shows a table only to an account with some privilege on
it, and a column only to one with a privilege on the column or above it:

- `SELECT` at the global, database, or table level covers every column, so
  without one the table metadata is incomplete (`columns_may_be_hidden`).
- A requested table `information_schema` does not show is "does not exist"
  when the account holds `SELECT` at the global or database level, and
  otherwise "not found": the table may exist and be hidden. No table is
  "permission denied", since `information_schema` hides a table rather than
  showing it without privilege.
- The privilege tables show only grants made to the account itself.
  Privileges held through roles cannot be confirmed, so a table readable only
  through a role is incomplete, and a table not shown is "not found".
- In a read, MySQL checks privileges before existence: `ER_NO_SUCH_TABLE`
  (1146) and `ER_BAD_FIELD_ERROR` (1054) are `does not exist`, while
  `ER_TABLEACCESS_DENIED_ERROR` (1142) and `ER_COLUMNACCESS_DENIED_ERROR`
  (1143), returned whether or not the target exists, are `permission
  denied`. A registered column that a table which may hide columns no longer
  shows is `not found`.
- The namespace list and the table list hold what the account can see.

For example, this table in database `metadata_test`:

```sql
CREATE TABLE filled (
  id bigint AUTO_INCREMENT PRIMARY KEY,
  parent_id bigint,
  code varchar(20) NOT NULL COMMENT 'Order code',
  amount int,
  doubled int AS (amount * 2) STORED,
  created_at timestamp DEFAULT CURRENT_TIMESTAMP,
  status enum('new', 'done') DEFAULT 'new',
  CONSTRAINT filled_parent FOREIGN KEY (parent_id) REFERENCES parent (id),
  CONSTRAINT amount_non_negative CHECK (amount >= 0),
  UNIQUE KEY code_prefix (code(4))
) COMMENT 'Order lines';
```

has this source metadata on MySQL 8.4.13, read by an account with `SELECT`
on the database:

```json
{
  "schema": "metadata_test",
  "name": "filled",
  "kind": "BASE TABLE",
  "comment": "Order lines",
  "definition": null,
  "columns_may_be_hidden": false,
  "columns": [
    {
      "name": "id",
      "ordinal_position": 1,
      "nullable": false,
      "data_source_type": "bigint",
      "data_type": "bigint",
      "character_set": null,
      "collation": null,
      "comment": null,
      "value_source": { "kind": "auto_increment" }
    },
    {
      "name": "parent_id",
      "ordinal_position": 2,
      "nullable": true,
      "data_source_type": "bigint",
      "data_type": "bigint",
      "character_set": null,
      "collation": null,
      "comment": null,
      "value_source": null
    },
    {
      "name": "code",
      "ordinal_position": 3,
      "nullable": false,
      "data_source_type": "varchar(20)",
      "data_type": "varchar",
      "character_set": "utf8mb4",
      "collation": "utf8mb4_0900_ai_ci",
      "comment": "Order code",
      "value_source": null
    },
    {
      "name": "amount",
      "ordinal_position": 4,
      "nullable": true,
      "data_source_type": "int",
      "data_type": "int",
      "character_set": null,
      "collation": null,
      "comment": null,
      "value_source": null
    },
    {
      "name": "doubled",
      "ordinal_position": 5,
      "nullable": true,
      "data_source_type": "int",
      "data_type": "int",
      "character_set": null,
      "collation": null,
      "comment": null,
      "value_source": { "kind": "generated", "generation": "stored", "expression": "(`amount` * 2)" }
    },
    {
      "name": "created_at",
      "ordinal_position": 6,
      "nullable": true,
      "data_source_type": "timestamp",
      "data_type": "timestamp",
      "character_set": null,
      "collation": null,
      "comment": null,
      "value_source": { "kind": "default_expression", "expression": "CURRENT_TIMESTAMP" }
    },
    {
      "name": "status",
      "ordinal_position": 7,
      "nullable": true,
      "data_source_type": "enum('new','done')",
      "data_type": "enum",
      "character_set": "utf8mb4",
      "collation": "utf8mb4_0900_ai_ci",
      "comment": null,
      "value_source": { "kind": "default", "value": "new" }
    }
  ],
  "primary_key": { "columns": ["id"] },
  "unique_keys": [
    {
      "name": "code_prefix",
      "keys": [{ "kind": "column", "column": "code", "prefix_length": 4 }]
    }
  ],
  "foreign_keys": [
    {
      "name": "filled_parent",
      "columns": ["parent_id"],
      "referenced": { "schema": "metadata_test", "table": "parent", "columns": ["id"] }
    }
  ],
  "checks": [{ "name": "amount_non_negative", "expression": "(`amount` >= 0)", "enforced": true }]
}
```

The document's shape can change between connector versions. Source metadata
stored before such a change cannot be compared with newly retrieved source
metadata ([Comparing table metadata](../../../docs/connector-contract.md#comparing-table-metadata)).

## Type mapping

The Data Source type is `COLUMN_TYPE` from `information_schema.COLUMNS`, such
as `int unsigned`, `decimal(10,2)`, or `enum('a','b')`, matched as
[Type mappings and coverage](../../../docs/connector-contract.md#type-mappings-and-coverage)
describes: the rule is looked up by `DATA_TYPE`, with `unsigned`, a
decimal's precision and scale, and an enum's members from `COLUMN_TYPE`. The
comparison column states how MySQL compares the type differently from the
[Analytics comparison](../../../docs/analytics-types.md#comparison), and the
last column which comparisons between two operands of the type run at MySQL
([Pushdown](#pushdown)):

| Data Source type | Analytics type | MySQL's comparison | Runs remotely |
| --- | --- | --- | --- |
| `tinyint`, `smallint`, `mediumint`, `int`, `bigint` | `Byte`, `SmallInt`, `Int`, `Int`, `BigInt`; `tinyint(1)` is also `Byte` by default | The same | Equality and ordering |
| `tinyint(1)` with `tinyint1_as_boolean` | `Boolean` | Compares `TRUE` and `FALSE` as 1 and 0 | Equality and ordering |
| `bit(1)` | `Boolean` | Compares `TRUE` and `FALSE` as 1 and 0 | Equality and ordering |
| `tinyint unsigned`, `smallint unsigned`, `mediumint unsigned`, `int unsigned`, `bigint unsigned` | `UnsignedByte`, `UnsignedSmallInt`, `UnsignedInt`, `UnsignedInt`, `UnsignedBigInt` | The same | Equality and ordering |
| `float`, `double` | `Float`, `Double` | The same; MySQL stores no NaN or infinity | None |
| `decimal(p,s)` | `Decimal(p, s)`; MySQL's precision is at most 65 | The same | Equality and ordering |
| `varchar(n)`, `tinytext`, `text`, `mediumtext`, `longtext` | `Text` | Follows the column's collation; under `utf8mb4_0900_bin`, a binary `NO PAD` collation, the same | Equality and ordering on columns under `utf8mb4_0900_bin`, with a constant without NUL or with another such column |
| `char(n)` | `Text` | MySQL drops trailing spaces when it reads the value; otherwise follows the collation | None |
| `binary(n)`, `varbinary(n)`, `tinyblob`, `blob`, `mediumblob`, `longblob` | `Blob` | The same; `binary(n)` values are padded with zero bytes | None |
| `date` | `Date` | The same | Equality and ordering, with a constant from year 1 to 9999 |
| `datetime(fsp)` | `Timestamp` | The same | Equality and ordering, with a constant from year 1 to 9999 |
| `timestamp(fsp)` | `TimestampTz` | The same under the fixed UTC session time zone | Equality and ordering, with a constant from year 1 to 9999 |
| `year` | `Year` | The same equality | None |
| `enum(...)` | `Enum` with the members in declaration order | Equality follows the enum's collation; ordering follows declaration | None |
| `json` | `Json` | MySQL compares JSON values | None |

MySQL has no boolean type: `BOOL` and `BOOLEAN` declare `tinyint(1)`, which
`COLUMN_TYPE` cannot tell from a `tinyint(1)` declared as such, and which
holds any `tinyint`, such as 2. By default it maps to `Byte`, which carries
every value. With `tinyint1_as_boolean` it maps to `Boolean`, as a schema
that declares `BOOLEAN` means, and a value other than 0 and 1 is a `data`
error. MySQL compares such a value as equal to neither `TRUE` nor `FALSE`,
so a filter that runs remotely and excludes its row, such as `= TRUE`, skips
it without failing, while one that returns its row, such as `<> TRUE`, fails
the read. `bit(1)` holds only 0 and 1 and is always `Boolean`.

Integers keep their width and signedness, and the driver returns each, and
each decimal in the narrowest Arrow decimal of its precision, as the
Analytics encoding, so those values pass through unconverted. `mediumint`
arrives in 32 bits, the narrowest Arrow integer that holds it.

`time`, `bit(n)` for n above 1, `set`, spatial types, and types added later,
such as `vector`, are not mapped. MySQL `time` represents elapsed time of up to ±838 hours
rather than a time of day, so its range does not fit `Time`.

Remote evaluation is narrower than agreement allows: floating-point, binary
string, and year comparisons, and enum equality, agree with the Analytics
comparison but stay in the local plan.

## Coverage

Version 0.1.0 covers every mapping rule in its [type mapping](#type-mapping).
The following values cannot be carried exactly and are `data` errors when a
read returns them:

- the zero date and datetime (`0000-00-00`), and dates with a zero month or
  day, which only a non-strict `sql_mode` stores;
- the zero year (`0000`);
- an enum's error value `''`, which a non-strict `sql_mode` stores for an
  invalid value; in an enum that declares `''` as a member it reads as that
  member;
- with `tinyint1_as_boolean`, a `tinyint(1)` other than 0 and 1.

## Pushdown

| Offered work | Runs remotely when |
| --- | --- |
| Filter | It combines comparisons and null tests with AND, OR, and NOT, and every comparison is between columns and constants of one type and runs remotely by the [type mapping](#type-mapping). The whole filter moves, or none of it does. |
| Projection | It selects plain columns and no filter remains local below it |

## Known limitations

This comes from the driver and is not yet handled by the connector:

- A zero date or datetime fails the read as a `data-source` error rather
  than a `data` error: the driver rejects the value while it builds a batch,
  and the error reaches the connector only as text.

The [conformance suite](#tests) checks it as a known deviation and fails once
the connector meets the contract.

Metadata requests cannot be made to wait at MySQL: it reads
`information_schema` from its data dictionary without locks, and neither
table locks nor pending DDL on the table or its database hold such reads.
The suite's checks that a cancelled or expired metadata request stops at the
source therefore do not run for this connector; cancellation and timeouts of
reads, which use the same driver cancellation, are checked.

## Compatibility

Only combinations that passed this crate's tests are listed.

| MySQL | ADBC driver | Platform |
| --- | --- | --- |
| 8.4.13 | 0.8.0 | macOS arm64 |
| 9.7.4 | 0.8.0 | macOS arm64 |

## Tests

The integration tests start a MySQL container with Docker and load the
driver from `ADBC_DRIVER_DIRECTORY`, which the Nix development shell sets.
`tests/conformance` runs the SDK's connector conformance suite with this
connector's fixture. The fixture reads with `tinyint1_as_boolean`, so its
reads cover the `Boolean` conversion; one metadata case reads with the
default options:

```sh
nix develop --command cargo test -p connector-mysql
```

`MYSQL_TEST_IMAGE` selects another server image, such as `mysql:9`. The
container is kept between runs and can be removed with `docker rm -f`.
