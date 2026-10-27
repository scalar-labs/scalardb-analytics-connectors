# Architecture

## Summary

ScalarDB Analytics Connector is the language-neutral connector contract and
Rust framework for integrating data sources with ScalarDB Analytics. The
repository separates the stable external contract, source-independent
processing, shared access mechanisms, source-specific behavior, and final C API
assembly.

A connector is the only component of ScalarDB Analytics that interprets a data
source. For one source kind, it retrieves metadata and decides each column's
Analytics type, and it reads rows as Arrow typed by those Analytics types. Query
Engines and the Physical Catalog use connectors through the connector API and
never touch data sources themselves.

The central structural rule is inward dependency: source-specific and
externally exposed crates depend on shared inner crates, while the shared
layers do not depend on a particular source or assembled connector.

## Product context

Connectors serve the other ScalarDB Analytics components, which live outside
this repository:

| Component | Unit | Responsibility | Does not hold |
| --- | --- | --- | --- |
| Connector | One per source kind; generic ODBC and Query Engine-backed connectors each handle several source kinds | Metadata retrieval, deciding Analytics types, obtaining current source facts, deciding whether a complete Substrait candidate is safe, translating accepted plans, and producing values typed by Analytics types | Query Engine native-plan knowledge |
| Adapter | One per Query Engine | Recognizing native planner patterns, constructing complete Substrait candidates from the current connector scan, applying planning outcomes to the native tree, retaining residual operators, and binding connector output into the Query Engine. Holds the mapping from Analytics types to Query Engine types, with its losses, as a table. | Data source semantics |
| Type conversion library | One | Converting between Analytics types and the schema carried by connector output; shared by the Physical Catalog and adapters | Decisions other than conversion |
| Physical Catalog | One | Persisting the Analytics types and source types connectors returned at registration, and serving them for browsing and planning | Type conversion |
| Query Engine | — | Mapping connector output to its own types and executing queries | Direct access to data sources |

Adding a data source adds no adapter, and adding a Query Engine adds no
connector.

Every column registered in the Physical Catalog carries an Analytics type, or is
an unsupported column, together with its source type. Logical plan construction,
catalog browsing, and cross-node references use those Analytics types without
contacting the source. Before connector pushdown callbacks begin, a separate
preparation operation hydrates current source facts used only to decide remote
execution safety. A Query Engine maps Analytics types to its own types through
its adapter. The reasons for this arrangement, including the properties it
guarantees and the alternatives it rejects, are recorded in
[Make one connector interpret each data source](design/make-one-connector-interpret-each-source.md).

A schema appears at three points, each with a distinct role:

| Point | Origin of the schema | Role |
| --- | --- | --- |
| Planning | The Physical Catalog's Analytics types, returned by the connector at registration | Plan without contacting the source |
| Read start | The Analytics types the connector derives now | Compared with the Physical Catalog's types by the [schema check](connector-contract.md#schema-check) |
| Read | The values the connector returns with the schema | Consistent, because one component decided the schema and produced the values |

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

The [connector contract](connector-contract.md) defines the language-neutral
semantics used across implementation languages. Public bindings and runtime
formats implement that contract without becoming a source-specific layer.

## Design priorities

1. **Language-neutral boundaries.** Connector requests, responses, declarations,
   and extension encodings have semantics independent of the Rust API.
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
| connector contract documentation | Language-neutral connector-boundary semantics |
| analytics-types | The source-independent Analytics schema model, validation, Arrow conversion, and extension encodings |
| connector-sdk | Source-independent connector behavior and the interface used by connector implementations |
| connector-adbc | Shared ADBC-based access behavior |
| connector-sql | Behavior shared by SQL-family connectors |
| Source connector | One source's catalog interpretation, type mappings, and source-specific processing |
| connector-api | C API implementation and assembly of the connector crates |

An outer component may select and compose capabilities from inner components.
An inner component must not inspect or branch on the identity of an outer
source. Connector crates do not depend on one another. The detailed
responsibilities and dependency rules are in
[Connector internals](connector-internals.md#structure-and-dependency-direction).

## Connector flow

A Query Engine adapter first prepares an immutable connector fact snapshot for
the registered source bindings. It then recognizes a native planner pattern and
uses the current connector scan to construct the complete next Substrait plan.
The SDK validates that candidate and binds adapter-owned requirements to facts
from the prepared snapshot. The connector decides whether the complete
candidate is supported. The adapter alone applies that outcome to the native
tree and owns any residual operator. The resulting `PlannedSubplan` crosses the
assembled connector API and is delegated to the selected source connector for
execution.

~~~text
native pattern + current connector scan
  -> adapter completes source-fact preparation before optimizer callbacks
  -> adapter builds the complete next Substrait candidate
  -> connector-sdk validates it and dispatches an operator-specific rule
  -> source connector uses prepared facts and makes a plan-aware decision
  -> adapter removes, retains, or leaves the native pattern
  -> source connector executes the accepted PlannedSubplan
  -> connector-sdk validates Arrow schema, lifecycle, and resource bounds
~~~

Substrait remains the shared relational and expression AST. The SDK does not
translate predicates into a second connector-specific expression tree or
evaluate residual predicates over returned Arrow batches. The detailed
planning decision is recorded in
[Plan pushdown with Query Engine planners](plan-pushdown-with-query-engine-planners.md).

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

The precise request and response shapes are owned by the
[connector contract](connector-contract.md). The shared type vocabulary and
Arrow conversion rules are owned by the
[Analytics type model](analytics-types.md) and enforced by `analytics-types`.

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
- One connector decides a data source's types and produces its values; no other
  component reinterprets or casts them.
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
- Static capability declarations are pruning hints; only a plan-aware connector
  rule can authorize a native planner rewrite.
- Serializable planned state contains no credential, open connection, or
  process-local pointer.
- Connector execution returns the schema recorded by `PlannedSubplan`; the SDK
  rejects mismatched batches and enforces cancellation, deadlines, and batch
  resource limits without adding an unbounded buffer.
- A column the source declares is never dropped: a column that cannot be mapped
  is returned as an unsupported column with its source type.

## Documentation map

| Document | Read it to |
| --- | --- |
| [Connector contract](connector-contract.md) | Call a connector, or check what a connector must guarantee |
| [Analytics type model](analytics-types.md) | Understand the shared type vocabulary and its Arrow encoding |
| [Type mappings](type-mappings.md) | Look up how a source kind's types map to Analytics types |
| [Plan pushdown with Query Engine planners](plan-pushdown-with-query-engine-planners.md) | Trace responsibility, state, planning, native rewrite, and execution across an adapter, the SDK, and a connector |
| [Query-engine pushdown integration](query-engine-pushdown.md) | Map Spark, Trino, DataFusion, or DuckDB planner hooks onto the shared pushdown model |
| [Connector internals](connector-internals.md) | Change the connector SDK, the connector API implementation, or crate structure |
| [Connector implementation conventions](connector-implementation.md) | Implement or change one source kind's connector |

Callers of connectors (adapters, the Physical Catalog, and the application)
need the contract and the type model. Connector implementers continue with the
type mappings, the internals, and the implementation conventions, in that
order.
