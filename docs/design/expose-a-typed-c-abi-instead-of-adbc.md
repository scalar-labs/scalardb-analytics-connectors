# Expose a typed C ABI instead of ADBC

## Design question

Should the connector API that callers use be ADBC, or an interface defined by
this repository?

## Decision

The connector API is a typed C ABI. Its first implementation publishes the C
header together with the functions and ABI tests. Read results cross it through
the Arrow C stream interface. Metadata and comparisons cross it as typed C
structures, returned whole by synchronous calls. ADBC remains one of the two
forms of Data Source access inside connectors; it is not the interface callers
see.

## Rationale

The callers are internal product components: Query Engine adapters and the
Physical Catalog. Letting external ADBC applications use a connector adds
little value.

Connectors accept no SQL text, and the contract's requests and table metadata
do not fit ADBC's functions. Carrying them over ADBC would require every caller
to implement agreements expressed as option strings and custom info codes. A
typed API states those agreements once in its public types and functions.

The same reasoning decides the form of each result. A read returns columnar
rows that can exceed memory and are consumed incrementally, which is what
Arrow streams carry. Metadata is a small, known structure that a caller holds
whole and passes back to a comparison. Carrying it as Arrow rows would make the
boundary a serialization format with no type for "table metadata", force
callers to rebuild Arrow arrays to compare what they retrieved, and encode
Analytics types in a second notation. Typed C structures state that shape
directly. Serializing metadata to store it or send it across a network is the
caller's choice, not the connector API's. Metadata requests return quickly and
are not on the query path, so they are synchronous calls bounded by a timeout
and a caller's cancellation; an asynchronous completion protocol would add
callbacks and threading rules without a caller that needs them.

## Consequences

The library publishes its own C header and versions it with the library as
described in
[Compatibility and versions](../connector-contract.md#compatibility-and-versions).
Language bindings for callers wrap this C ABI rather than an ADBC driver
manager.
