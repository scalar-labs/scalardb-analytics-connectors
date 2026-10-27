# Type mappings

## Summary

A type mapping defines, for one source kind, where source types are read from,
in what form they are returned, and which Analytics type each maps to. This
document holds the current mappings. How a connector declares coverage over
them, and how columns outside its coverage are returned, follow
[Type mappings and coverage](connector-contract.md#type-mappings-and-coverage)
and [Unsupported columns](connector-contract.md#unsupported-columns). Analytics
type names follow the [Analytics type model](analytics-types.md). These tables
are the canonical source mappings.

## Common rules

Mappings are defined for eight source kinds: PostgreSQL, MySQL, SQLite,
Parquet, Iceberg, Delta Lake, SQL Server, and Oracle. A type mapping is the
mapping guaranteed by metadata retrieval and data read; it does not widen the
set of data sources the product allows to be registered. Changing a mapping
rule raises the version of every connector that covers it
([Type mappings and coverage](connector-contract.md#type-mappings-and-coverage)).

Source types are read from:

| Source kind | Origin of the source type |
| --- | --- |
| PostgreSQL | The result of `format_type(atttypid, atttypmod)` over `pg_catalog`, such as `character varying(10)` or `timestamp with time zone` |
| MySQL | `COLUMN_TYPE` in `information_schema.COLUMNS`, such as `int unsigned`, `decimal(10,2)`, or `enum('a','b')` |
| SQLite | The declared type in the table definition (`type` in `PRAGMA table_info`) |
| Parquet | The physical type and logical type annotation in the footer's schema elements |
| Iceberg | The type in the table metadata's schema |
| Delta Lake | The type in the schema of the transaction log's `metaData` |
| SQL Server | The type name, length, precision, and scale from `sys.columns` and `sys.types` |
| Oracle | `DATA_TYPE`, `DATA_PRECISION`, and `DATA_SCALE` in `ALL_TAB_COLUMNS` |

Every type not listed in a table is not mapped and becomes an unsupported
column. The left column of each table is the type name reported by the origin,
matched case-insensitively; parameters such as `(p, s)` follow the conditions in
the right column. Only extension-encoded types registered in the
[Analytics type model](analytics-types.md#registered-extension-encoded-types)
are used. Values that cannot be carried exactly, such as PostgreSQL `numeric`
NaN, are a `data` error in data read under any rule
([Values](connector-contract.md#values)).

Among the types the Analytics type model does not admit, unsigned 64-bit
integers, decimals with precision above 38, and decimals without a declared
precision become unsupported columns. The rules that map unsigned integers of
32 bits or fewer to the next wider signed integer (MySQL, Parquet, SQL Server)
also rely on unsigned integers being outside the Analytics types. If those
types are admitted, the affected rules are added or changed and the covering
connectors raise their versions.

## PostgreSQL

| Source type | Analytics type |
| --- | --- |
| `boolean` | `Boolean` |
| `smallint`, `integer`, `bigint` | `SmallInt`, `Int`, `BigInt` |
| `real`, `double precision` | `Float`, `Double` |
| `numeric(p,s)` | `Decimal(p, s)` when p is at most 38; `numeric` without precision is unsupported |
| `text`, `character varying(n)`, `character(n)` | `Text` |
| `bytea` | `Blob` |
| `date` | `Date` |
| `time without time zone` | `Time` |
| `timestamp without time zone` | `Timestamp` |
| `timestamp with time zone` | `TimestampTz` |
| `interval` | `Interval` |
| `uuid` | `Uuid` |
| `json`, `jsonb` | `Json` |
| User-defined enum | `Enum` |
| `time with time zone` | `TimeWithTimeZone` |
| Domain over one of the types above | The same as its base type; the source type keeps the domain's name |

Arrays are not mapped. PostgreSQL's catalog does not enforce an array's number
of dimensions, so the nesting depth of `Array(T)` cannot be determined from the
schema.

## MySQL

| Source type | Analytics type |
| --- | --- |
| `TINYINT`, `SMALLINT`, `MEDIUMINT`, `INT`, `BIGINT` | `Byte`, `SmallInt`, `Int`, `Int`, `BigInt`; `TINYINT(1)` is also `Byte` |
| `TINYINT UNSIGNED`, `SMALLINT UNSIGNED`, `MEDIUMINT UNSIGNED`, `INT UNSIGNED` | `SmallInt`, `Int`, `Int`, `BigInt`: the next wider signed integer that holds the range |
| `FLOAT`, `DOUBLE` | `Float`, `Double` |
| `DECIMAL(p, s)` | `Decimal(p, s)` when p is at most 38 |
| `CHAR`, `VARCHAR`, `TINYTEXT`, `TEXT`, `MEDIUMTEXT`, `LONGTEXT` | `Text` |
| `BINARY`, `VARBINARY`, `TINYBLOB`, `BLOB`, `MEDIUMBLOB`, `LONGBLOB` | `Blob` |
| `DATE` | `Date` |
| `DATETIME` | `Timestamp` |
| `TIMESTAMP` | `TimestampTz` |
| `YEAR` | `Year` |
| `ENUM` | `Enum` |
| `JSON` | `Json` |

`TINYINT(1)` is not a boolean. Treating its display width as a boolean
declaration is a JDBC driver's reinterpretation: MariaDB Connector/J reports
`TINYINT(1)` as `BOOLEAN` under its default settings, so a mapping based on
that report changes when the driver setting changes. Reading `COLUMN_TYPE` from
`information_schema` removes that dependence, and the integer stays `Byte`.

`BIGINT UNSIGNED`, `TIME`, `BIT`, `SET`, and spatial types are not mapped.
MySQL `TIME` represents elapsed time of up to ±838 hours rather than a time of
day, so its range does not fit `Time`.

## SQLite

SQLite derives a type affinity from a column's declared type and does not
enforce value types. The mapping uses the declared type's affinity.

| Affinity of the declared type | Analytics type |
| --- | --- |
| INTEGER | `BigInt` |
| TEXT | `Text` |
| BLOB (declared type contains `BLOB`) | `Blob` |
| REAL | `Double` |
| NUMERIC (including `DECIMAL`, `BOOLEAN`, `DATE`, `DATETIME`) | Unsupported |

A column without a declared type also has BLOB affinity, but values of any type
can be stored in it, so it is unsupported.

Table metadata follows the declarations. A value whose type differs from its
declaration appears as a failure during data read and does not change table
metadata.

## Parquet

The source type is the pair of physical type and logical type annotation in a
Parquet file's footer.

| Source type | Analytics type |
| --- | --- |
| `BOOLEAN` | `Boolean` |
| `INT32`, `INT64`, signed `INT(8)`, signed `INT(16)` | `Int`, `BigInt`, `Byte`, `SmallInt` |
| Unsigned `INT(8)`, `INT(16)`, `INT(32)` | `SmallInt`, `Int`, `BigInt` |
| `FLOAT`, `DOUBLE` | `Float`, `Double` |
| `DECIMAL(p, s)` | `Decimal(p, s)` when p is at most 38 |
| `STRING`, `ENUM` | `Text` |
| `BYTE_ARRAY` and `FIXED_LEN_BYTE_ARRAY` without annotation | `Blob` |
| `DATE` | `Date` |
| `TIME` (milliseconds or microseconds, not adjusted to UTC) | `Time` |
| `TIMESTAMP` (milliseconds or microseconds, not adjusted to UTC) | `Timestamp` |
| `TIMESTAMP` (milliseconds or microseconds, adjusted to UTC) | `TimestampTz` |
| `UUID` | `Uuid` |
| `JSON` | `Json` |
| `LIST`, `MAP`, group | `Array`, `Map`, `Struct` |

Nanosecond `TIME` and `TIMESTAMP`, UTC-adjusted `TIME`, `INT96`, `FLOAT16`, and
unsigned `INT(64)` are not mapped. The `ENUM` annotation carries no member list,
so it maps to `Text` rather than `Enum`.

## Iceberg

| Source type | Analytics type |
| --- | --- |
| `boolean` | `Boolean` |
| `int`, `long` | `Int`, `BigInt` |
| `float`, `double` | `Float`, `Double` |
| `decimal(P, S)` | `Decimal(P, S)` |
| `string` | `Text` |
| `binary`, `fixed(L)` | `Blob` |
| `date` | `Date` |
| `time` | `Time` |
| `timestamp` | `Timestamp` |
| `timestamptz` | `TimestampTz` |
| `uuid` | `Uuid` |
| `list`, `map`, `struct` | `Array`, `Map`, `Struct` |

`timestamp_ns`, `timestamptz_ns`, `variant`, `geometry`, `geography`, and
`unknown` are not mapped.

## Delta Lake

| Source type | Analytics type |
| --- | --- |
| `boolean` | `Boolean` |
| `byte`, `short`, `integer`, `long` | `Byte`, `SmallInt`, `Int`, `BigInt` |
| `float`, `double` | `Float`, `Double` |
| `decimal(p, s)` | `Decimal(p, s)` |
| `string` | `Text` |
| `binary` | `Blob` |
| `date` | `Date` |
| `timestamp` | `TimestampTz` |
| `timestamp_ntz` | `Timestamp` |
| `array`, `map`, `struct` | `Array`, `Map`, `Struct` |

Delta Lake `timestamp` represents an instant, so it maps to `TimestampTz`.
`variant` is not mapped.

## SQL Server

| Source type | Analytics type |
| --- | --- |
| `bit` | `Boolean` |
| `tinyint` | `SmallInt`; `tinyint` ranges from 0 to 255 |
| `smallint`, `int`, `bigint` | `SmallInt`, `Int`, `BigInt` |
| `real`, `float` | `Float`, `Double`; `float(n)` is `Float` when n is at most 24 |
| `decimal(p, s)`, `numeric(p, s)` | `Decimal(p, s)` |
| `char`, `varchar`, `nchar`, `nvarchar`, `text`, `ntext` | `Text` |
| `binary`, `varbinary`, `image` | `Blob` |
| `date` | `Date` |
| `time(n)` | `Time` when n is at most 6 |
| `datetime2(n)` | `Timestamp` when n is at most 6 |
| `smalldatetime` | `Timestamp` |
| `datetimeoffset(n)` | `TimestampTz` when n is at most 6 |
| `uniqueidentifier` | `Uuid` |

The default precision of `time`, `datetime2`, and `datetimeoffset` is 7
(100 nanoseconds), so columns without a declared precision are unsupported.
`datetime` holds values in 1/300-second steps, which microseconds cannot
represent exactly, so it is not mapped. `money`, `smallmoney`, `xml`,
`sql_variant`, `hierarchyid`, `rowversion`, and spatial types are not mapped
either.

## Oracle

| Source type | Analytics type |
| --- | --- |
| `NUMBER(p, s)` | `Decimal(p, s)` when s is between 0 and p; `NUMBER` without precision is unsupported |
| `BINARY_FLOAT`, `BINARY_DOUBLE` | `Float`, `Double` |
| `BOOLEAN` | `Boolean` |
| `CHAR`, `NCHAR`, `VARCHAR2`, `NVARCHAR2`, `CLOB`, `NCLOB` | `Text` |
| `RAW`, `BLOB` | `Blob` |
| `DATE` | `Timestamp`; Oracle `DATE` includes a time of day to the second |
| `TIMESTAMP(n)` | `Timestamp` when n is at most 6 |
| `TIMESTAMP(n) WITH TIME ZONE`, `TIMESTAMP(n) WITH LOCAL TIME ZONE` | `TimestampTz` when n is at most 6 |

`NUMBER(p, 0)` also maps to `Decimal(p, 0)` rather than an integer type.
`FLOAT(b)`, `INTERVAL YEAR TO MONTH`, `INTERVAL DAY TO SECOND`, `LONG`, `ROWID`,
`XMLTYPE`, `JSON`, and spatial types are not mapped.
