# Expose a typed C ABI instead of ADBC

## Design question

Should the connector API that callers use be ADBC, or an interface defined by
this repository?

## Decision

The connector API is a typed C ABI. Its first implementation publishes the C
header together with the functions and ABI tests. Results cross it through the
Arrow C data interface. ADBC remains one of the two forms of Data Source access
inside connectors; it is not the interface callers see.

## Rationale

The callers are internal product components: Query Engine adapters and the
Physical Catalog. Letting external ADBC applications use a connector adds
little value.

Connectors accept no SQL text, and the contract's requests and table metadata
do not fit ADBC's functions. Carrying them over ADBC would require every caller
to implement agreements expressed as option strings and custom info codes. A
typed API states those agreements once in its public types and functions.

## Consequences

The library publishes its own C header and versions it with the library as
described in
[Compatibility and versions](../connector-contract.md#compatibility-and-versions).
Language bindings for callers wrap this C ABI rather than an ADBC driver
manager.
