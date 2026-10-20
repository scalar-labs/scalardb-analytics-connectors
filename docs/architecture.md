# Architecture

## Summary

ScalarDB Analytics Connector is the language-neutral connector contract and
Rust framework for integrating data sources with ScalarDB Analytics. The
repository separates the stable external contract, source-independent
processing, shared access mechanisms, source-specific behavior, and final C API
assembly.

The central structural rule is inward dependency: source-specific and
externally exposed crates depend on shared inner crates, while the shared
layers do not depend on a particular source or assembled connector.

## System shape

The Rust workspace has the following dependency direction:

~~~text
connector-api
    |
    +--> connector-postgresql --> connector-sql --> connector-adbc
    |                                      |              |
    +--> connector-mysql ------------------+              v
    |                                               connector-sdk
    +---------------------------------------------->      |
                                                          v
                                                   analytics-types
~~~

The language-neutral files under contract define the interface artifacts used
across implementation languages. They are validated independently from the Rust
crate graph and do not become a source-specific implementation layer.

## Design priorities

1. **Language-neutral boundaries.** Connector requests, responses, declarations,
   and extension encodings have contract artifacts outside the Rust API.
2. **Source isolation.** PostgreSQL, MySQL, and future source behavior remains in
   its connector or an explicitly shared access layer.
3. **Inward dependencies.** Shared schema and SDK crates do not depend on
   source-specific connectors or the final C API assembly.
4. **Explicit type translation.** Source, Analytics, Arrow, and Query Engine
   type systems meet through owned mappings rather than a shared accidental
   representation.
5. **Portable implementation.** The workspace keeps native driver dependencies
   outside the source-independent SDK and declares one verified Rust MSRV.

## Component ownership

| Component | Owns |
| --- | --- |
| contract | Connector interface artifacts and language-neutral Arrow extension encodings |
| analytics-types | The source-independent Analytics schema model, validation, and Arrow conversion |
| connector-sdk | Source-independent connector behavior and the interface used by connector implementations |
| connector-adbc | Shared ADBC-based access behavior |
| connector-sql | Behavior shared by SQL-family connectors |
| Source connector | One source's catalog interpretation, type mappings, and source-specific processing |
| connector-api | C API implementation and assembly of the connector crates |

An outer component may select and compose capabilities from inner components.
An inner component must not inspect or branch on the identity of an outer
source. Connector crates do not depend on one another.

## Connector flow

A request enters through the assembled connector API and is delegated to the
selected source connector. The connector uses shared SQL, ADBC, and SDK
capabilities where applicable, while retaining ownership of source catalog
interpretation and source-specific mappings.

Schema information crosses the source boundary as an Analytics schema and is
serialized through Arrow for interchange:

~~~text
caller
  -> connector-api
  -> source connector
  -> shared SQL / ADBC / SDK capabilities
  -> data source

data source schema
  -> source-specific mapping
  -> Analytics schema
  -> Arrow schema at the connector boundary
~~~

The precise request and response shapes are owned by contract. The shared type
vocabulary and Arrow conversion rules are owned by the
[Analytics type model](analytics-types.md).

## Type-system boundary

Analytics types form the source-independent federation schema. Source types,
Arrow types, and Query Engine types are concrete boundary representations:

~~~text
Data source type
      |
      | connector-specific mapping
      v
Analytics type
      |
      | shared Arrow encoding
      v
Arrow type
      |
      | adapter-specific mapping
      v
Query Engine type
~~~

This flow is cross-cutting infrastructure rather than the top-level purpose of
the repository. Arrow storage and extension details do not define a hierarchy
inside the Analytics type system.

## Boundaries and invariants

- The contract is language-neutral and does not derive its meaning from Rust
  implementation details.
- Dependencies point from assembly and source-specific crates toward shared
  crates, never in the reverse direction.
- Source-specific catalog notation and behavior remain outside analytics-types
  and connector-sdk.
- Native driver dependencies belong to independently built driver artifacts,
  not connector-adbc, connector-sdk, or source connector crates.
- Every schema field has one Analytics type; there is no physical-versus-logical
  layer in the federation schema.
- Arrow representation details belong to the Analytics-to-Arrow conversion
  boundary.
- Unsupported source or Arrow representations fail explicitly rather than
  silently changing the Analytics type.
