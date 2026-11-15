# Analytics type model

## Summary

The Analytics type model is the canonical schema vocabulary shared across
connectors, catalog processing, Arrow interchange, and Query Engine adapters.
It unifies those systems without adopting any one system's internal type
hierarchy. The Physical Catalog persists Analytics types, and connector output
is typed by them.

AnalyticsType is one semantic type system. Scalar types such as Int and Text,
structured types such as Array and Struct, and types such as Uuid and Enum are
peer variants. The reason for this shape is recorded in
[Model Analytics types as one federation type system](design/model-analytics-types-as-one-system.md).

## Boundary mappings

Type conversion is owned by the boundary where the two type systems meet:

| Boundary | Mapping owner | Example |
| --- | --- | --- |
| Data Source to Analytics | Connector, through its [type mapping](connector-contract.md#type-mappings-and-coverage) | PostgreSQL uuid to Analytics Uuid |
| Analytics to Arrow | analytics-types | Analytics Uuid to Arrow Utf8 plus extension metadata |
| Analytics to Query Engine | Query Engine adapter ([Execution](#execution)) | Analytics Uuid to DuckDB UUID |

A PostgreSQL text column maps to Analytics Text; it does not become Uuid
because both happen to use Arrow Utf8. Conversely, PostgreSQL uuid, Parquet UUID
annotation, and SQL Server uniqueidentifier may all map to the same Analytics
Uuid even though their Data Source type systems differ.

Connector output crosses to adapters as Arrow for two reasons: DuckDB,
DataFusion, and Spark all ingest Arrow, which keeps adapter mappings small, and
Arrow extension types carry meanings a storage type alone cannot. The
Physical Catalog and adapters share the Analytics-to-Arrow conversion.

## Types

Each Analytics type has one Arrow encoding and one comparison. Types with a
direct Arrow encoding use exactly one storage type; extension-encoded types add
extension metadata to a storage type
([Extension-encoded types](#extension-encoded-types)). Only types Arrow can
represent are admitted, because a type Arrow cannot represent could not reach
every adapter without conversion.

| Analytics type | Arrow encoding | Equality | Ordering |
| --- | --- | --- | --- |
| Boolean | Bool | The same value | false before true |
| Byte, SmallInt, Int, BigInt | Int8, Int16, Int32, Int64 | The same number | Numeric |
| UnsignedByte, UnsignedSmallInt, UnsignedInt, UnsignedBigInt | UInt8, UInt16, UInt32, UInt64 | The same number | Numeric |
| Float, Double | Float32, Float64 | The same number; -0 equals +0, and NaN equals NaN | Numeric, with NaN after every other value, including positive infinity |
| Decimal(p, s), p from 1 to 76 | The narrowest Arrow decimal that holds p digits: Decimal32(p, s) up to 9, Decimal64(p, s) up to 18, Decimal128(p, s) up to 38, and Decimal256(p, s) up to 76 | The same number | Numeric |
| Text | Utf8 | The same UTF-8 bytes; no normalization, case folding, or padding, so trailing spaces count | By UTF-8 bytes, which is code point order; a string precedes every longer string it begins |
| Blob | Binary | The same bytes | By bytes; a value precedes every longer value it begins |
| Date | Date32 | The same date | Chronological |
| Time | Time64 in microseconds | The same time of day | Chronological |
| Timestamp | Timestamp in microseconds without a time zone | The same date and time | Chronological |
| TimestampTz | Timestamp in microseconds with UTC | The same instant | Chronological by instant |
| Duration | Duration in microseconds | The same length | By length |
| Interval | Interval in MonthDayNano | The same months, days, and nanoseconds each, so 1 month and 30 days differ | Not defined: months and days have no fixed length |
| Array(T) | List of T | Not defined | Not defined |
| Map(K, V) | Map of K to V | Not defined | Not defined |
| Struct(f1: T1, ...) | Struct of f1: T1, ... | Not defined | Not defined |
| Uuid | Utf8, extension `scalardb.analytics.uuid` | The same 36-character canonical text | Not defined |
| Json | Utf8, extension `scalardb.analytics.json` | The same JSON text, which is the same JSON value only when the source emits canonical form | Not defined |
| Enum | Utf8, extension `scalardb.analytics.enum` | The same member label | Not defined |
| Year | Int16, extension `scalardb.analytics.year` | The same year | Not defined |
| TimeWithTimeZone | Utf8, extension `scalardb.analytics.timetz` | The same time and zone, so `10:00:00+09` and `01:00:00+00` differ | Not defined |

Each encoding is the Arrow type a source's values are naturally in, so a
connector whose Data Source access returns that type passes it through
without converting values or using more space than they need. That is why
unsigned integers keep their own types rather than widening to signed ones,
and why a decimal's width follows its precision rather than being fixed. One
encoding per type keeps it the same for every connector, so an adapter
recognizes a type by its Arrow encoding alone. Decimals beyond Arrow's 76
digits, such as PostgreSQL `numeric` without a declared precision, are not
admitted ([Type mappings and coverage](connector-contract.md#type-mappings-and-coverage)).

### Comparison

The Equality and Ordering columns define each type's comparison: which values
are equal and, where the type has one, how values are ordered. An offered
filter compares with this meaning, and a Connector rewrite rule preserves it
([Filters and types](connector-contract.md#filters-and-types)). A comparison
with NULL is NULL, following SQL three-valued logic.

The floating-point order is the one PostgreSQL, DuckDB, and Spark use, so their
comparisons need no correction. IEEE 754 comparison, under which NaN equals
nothing, is a difference from it.

Comparing the storage values of an extension-encoded type implements its
equality. A Query Engine that does not recognize the extension compares
storage values, so they must give the type's equality. When jsonb or JSON is
not emitted in canonical form, string equality differs from semantic equality.
A type whose equality holds only conditionally is still extension-encoded,
with the condition stated in its definition: a type with a direct encoding
would not make Query Engines implement JSON semantic equality, so it would
bring no benefit.

A comparison the table does not define has no Analytics meaning, so an adapter
does not offer it. The comparison semantics of a Data Source or Query Engine do
not define an Analytics type's comparison either. Sources compare the same kind
of value differently: one source compares time-with-time-zone values by time
and zone, as PostgreSQL does, while another may compare them after conversion
to UTC. Both map to TimeWithTimeZone rather than to separate types, because one
meaning per Analytics type is what lets values from different sources be
compared and joined. Each adapter's mapping table states how its Query Engine
compares differently and which comparisons it offers ([Execution](#execution)).
Each Connector's type mapping states how its Data Source compares differently
and which comparisons it evaluates there; a comparison the source evaluates
differently stays in the local plan
([Plan-aware support decisions](connector-implementation.md#plan-aware-support-decisions)).

## Extension-encoded types

Types other than those with a direct encoding use namespaced Arrow extension
metadata because their Analytics meaning is not expressed by an Arrow storage
type alone. These are called extension-encoded types. The distinction is about
Arrow representation, not a layer inside the Analytics types.

### Registered extension-encoded types

Extension-encoded types are registered here, on the product side, and
connectors use only registered ones. A connector that needs a new one registers
it here first, then maps to it in its
[type mapping](connector-contract.md#type-mappings-and-coverage).
Registration is required because adapter mapping tables and expression planning
([Filters and types](connector-contract.md#filters-and-types)) look types up by
extension name, so one name must mean the same thing in every connector. A
connector that returns an unregistered extension-encoded type fails the
[metadata conformance tests](connector-contract.md#metadata-conformance-tests).

| Analytics type | Extension name | Parameters | Storage type | Meaning |
| --- | --- | --- | --- | --- |
| Uuid | scalardb.analytics.uuid | None | Utf8 | A UUID; the value is its 36-character canonical form |
| Json | scalardb.analytics.json | None | Utf8 | A JSON value; the value is the JSON text the source emitted. Equality holds only when the source emits canonical form. |
| Enum | scalardb.analytics.enum | Ordered member labels | Utf8 | An enumeration; the value is the member label |
| Year | scalardb.analytics.year | None | Int16 | A year |
| TimeWithTimeZone | scalardb.analytics.timetz | None | Utf8 | A time of day with a time zone, such as `10:00:00+09`; equal only when both time and zone are equal |

This table defines the stable names, parameter shapes, value representations,
equality conditions, and storage encodings. The `analytics-types` conversion
implementation enforces these encodings in both directions. They describe a
serialization boundary, not a category or hierarchy inside AnalyticsType.

### Extension metadata

An extension-encoded type sets `ARROW:extension:name` to its extension name,
prefixed with `scalardb.analytics.`, and `ARROW:extension:metadata` to its
parameters as JSON. For example:

~~~text
Analytics Uuid
  -> storage type: Utf8
  -> ARROW:extension:name = scalardb.analytics.uuid
  -> ARROW:extension:metadata = {}

Analytics Enum(["pending", "done"])
  -> storage type: Utf8
  -> ARROW:extension:name = scalardb.analytics.enum
  -> ARROW:extension:metadata = {"members":["pending","done"]}
~~~

An Arrow implementation that does not recognize the extension name can read
the storage value. This is a compatibility fallback at the interchange
boundary; it does not make Text the semantic parent of Uuid or Enum. It also
lets an adapter whose Query Engine has no native equivalent map the type to the
storage value without changing the Query Engine: the DuckDB adapter maps Uuid
to UUID, while a Spark adapter maps it to a string.

On reverse conversion, an extension name must be defined by the contract, its
parameters must match the defined shape, and its Arrow storage type must match
the declared encoding. Otherwise conversion returns a typed error. Equivalent
JSON parameter spellings are parsed into the same Analytics value and serialize
back to one canonical representation; raw Arrow metadata spelling is not part
of Analytics equality.

## Structured types and validation

AnalyticsSchema contains ordered AnalyticsField values. A field owns its name,
AnalyticsType and nullability. Struct fields use the same shape recursively,
the same shape as table columns: ordered, named, and typed. Array and Map have
a fixed number of type parameters (one and two), while only Struct has an
arbitrary number of fields. Arrays preserve element nullability; maps require
non-null keys and preserve value nullability. Arrow-only child names and
metadata are validated or canonicalized by the conversion boundary and are not
stored in these types.

DecimalType validates the precision and scale representable by the selected
Arrow decimal encoding. TimestampTz uses the established UTC Arrow timestamp
encoding.

## Adding Analytics types

Adding an Analytics type, whether extension-encoded or directly encoded, is
released together with its callers, because the library and its callers are
updated together
([Compatibility and versions](connector-contract.md#compatibility-and-versions)).
Every adapter adds one row to its mapping table for the new type.

## Execution

At run time, connector Arrow output passes through an adapter into the Query
Engine:

~~~text
connector --Arrow stream--> adapter --> Query Engine Arrow ingestion --> Query Engine types
                              |
                              +- maps the planned Analytics types to Query Engine types
                                 through the adapter's mapping table; fails if a different type arrives
~~~

An adapter holds one mapping table keyed by Analytics type. For every Analytics
type the adapter knows, it defines the target Query Engine type, the loss of
that mapping, and the comparisons the adapter offers with how the Query
Engine's comparison differs from the type's ([Comparison](#comparison)):

| Analytics type | DuckDB adapter | Spark adapter |
| --- | --- | --- |
| TimestampTz | TIMESTAMP WITH TIME ZONE, lossless | TimestampType, lossless |
| Uuid | UUID, lossless | StringType; the type's meaning is lost and values are handled as strings |
| Enum | ENUM, lossless | StringType; the member set and order are lost |

The table must be complete. The library and its adapters are updated
together, so an adapter never receives a type missing from its table. The
DuckDB adapter's complete table is in its
[README](../adapters/duckdb/README.md#type-mapping). Everything else, such as precision and scale, timestamp unit and time
zone, nullability, and nesting, is carried by the Arrow schema.

### Unexpected types

An adapter converts the schema it receives at the head of the stream into Query
Engine types according to the Analytics types used for planning. If a
column name, storage type or its parameters, extension name, or nullability
differs from the planned type, conversion fails and so does the query. This
failure happens after the connector's
[schema check](connector-contract.md#schema-check) passed, so it is treated as
a connector defect, not as a stale registration.

### Extension types through expressions

An extension-encoded type survives while a column is projected unchanged. After
an expression, the result has the Query Engine's type: a Uuid column through a
substring function becomes a string, and a Year column after addition becomes an
integer. This is the general behavior of Arrow extension types, and ScalarDB
Analytics does not override it. Query Engines may also drop extension metadata
when returning results, as Spark does.

## Unsupported columns

A column whose Data Source type a connector cannot map to an Analytics type is still
registered in the Physical Catalog with its name, ordinal, nullability, and
Data Source type, and without an Analytics type. So is a column whose Analytics type,
as returned by the connector, is unknown to the Physical Catalog.

Unsupported is not a type. Analytics types are the set of types ScalarDB
Analytics has declared it can carry; making a column it cannot carry a member
would make "declared" and "readable" disagree. Projecting an unsupported column
fails during planning, and unsupported columns never appear in a connector's
stream.

The Data Source type also records what the connector's mapping did not cover, and
decisions to add mappings are made from that record.

## Inferred types

For data sources without declared schemas, such as DynamoDB, Cosmos DB, and JSON
files, a connector infers types by examining values. An inferred Analytics type
does not copy a declaration, so the Physical Catalog distinguishes whether a
column's Analytics type was declared or inferred.

## Invariants

- A field has one Analytics type, regardless of how that type is encoded.
- Arrow representation choices do not create subtype relationships inside the
  Analytics model.
- An extension name and parameter shape mean the same thing for every
  connector and adapter, and connectors use only registered extension-encoded
  types.
- Data Source-specific catalog details do not appear in AnalyticsType.
- Supported nested schemas round-trip without losing Analytics names, ordering,
  nullability, or type parameters.
- Unknown extension names, malformed parameters, encoding mismatches, and
  unsupported Arrow types are distinct typed failures.
