# Analytics type model

## Summary

The Analytics type model is the canonical schema vocabulary shared across
connectors, catalog processing, Arrow interchange, and Query Engine adapters.
It unifies those systems without adopting any one system's internal type
hierarchy.

AnalyticsType is one semantic type system. Scalar types such as Int and Text,
structured types such as Array and Struct, and types such as Uuid and Enum are
peer variants. The reason for this shape is recorded in
[Model Analytics types as one federation type system](design/model-analytics-types-as-one-system.md).

## Boundary mappings

Type conversion is owned by the boundary where the two type systems meet:

| Boundary | Mapping owner | Example |
| --- | --- | --- |
| Source to Analytics | Source connector | PostgreSQL uuid to Analytics Uuid |
| Analytics to Arrow | analytics-types | Analytics Uuid to Arrow Utf8 plus extension metadata |
| Analytics to Query Engine | Query Engine adapter | Analytics Uuid to DuckDB UUID |

A PostgreSQL text column maps to Analytics Text; it does not become Uuid
because both happen to use Arrow Utf8. Conversely, PostgreSQL uuid, Parquet UUID
annotation, and SQL Server uniqueidentifier may all map to the same Analytics
Uuid even though their source type systems differ.

## Arrow encodings

Some Analytics types have direct Arrow encodings, including booleans, signed
integers, floating-point numbers, decimals, text, binary values, temporal
types, arrays, maps, and structs. Other peer types use namespaced Arrow
extension metadata because their Analytics meaning is not expressed by an
Arrow storage type alone:

| Analytics type | Parameters | Arrow representation |
| --- | --- | --- |
| Uuid | None | Utf8 plus scalardb.analytics.uuid |
| Json | None | Utf8 plus scalardb.analytics.json |
| Enum | Ordered member labels | Utf8 plus scalardb.analytics.enum |
| Year | None | Int16 plus scalardb.analytics.year |
| TimeWithTimeZone | None | Utf8 plus scalardb.analytics.timetz |

The machine-readable
[Arrow extension contract](../contract/analytics-types/arrow-extensions.json)
records stable names, parameter shapes, value representations, equality
conditions, and storage encodings. It describes a serialization boundary, not
a category or hierarchy inside AnalyticsType.

## Arrow encoding

Arrow storage types and extension metadata are interchange details. For
example:

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
the storage value as text. This is a compatibility fallback at the interchange
boundary; it does not make Text the semantic parent of Uuid or Enum.

On reverse conversion, an extension name must be defined by the contract, its
parameters must match the defined shape, and its Arrow storage type must match
the declared encoding. Otherwise conversion returns a typed error. Equivalent
JSON parameter spellings are parsed into the same Analytics value and serialize
back to one canonical representation; raw Arrow metadata spelling is not part
of Analytics equality.

## Structured types and validation

AnalyticsSchema contains ordered AnalyticsField values. A field owns its name,
AnalyticsType and nullability. Struct fields use the same shape recursively.
Arrays preserve element nullability; maps require non-null keys and preserve
value nullability. Arrow-only child names and metadata are validated or
canonicalized by the conversion boundary and are not stored in these types.

DecimalType validates the precision and scale representable by the selected
Arrow decimal encoding. Unsupported unsigned integers and decimal precision
above 38 are not admitted by the current model. TimestampTz uses the
established UTC Arrow timestamp encoding.

## Invariants

- A field has one Analytics type, regardless of how that type is encoded.
- Arrow representation choices do not create subtype relationships inside the
  Analytics model.
- An extension name and parameter shape mean the same thing for every
  connector and adapter.
- Source-specific catalog details do not appear in AnalyticsType.
- Supported nested schemas round-trip without losing Analytics names, ordering,
  nullability, or type parameters.
- Unknown extension names, malformed parameters, encoding mismatches, and
  unsupported Arrow types are distinct typed failures.
