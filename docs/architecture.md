# Architecture

## Summary

ScalarDB Analytics Connector is the language-neutral connector contract and
Rust framework for integrating data sources with ScalarDB Analytics. The
repository separates the stable external contract, source-independent
processing, shared access mechanisms, Data Source-specific behavior, and final C API
assembly.

A connector is the only component of ScalarDB Analytics that interprets a data
Data Source. For one Connector, it retrieves metadata and decides each column's
Analytics type, and it reads rows as Arrow typed by those Analytics types. Query
Engines and the Physical Catalog use connectors through the connector API and
never touch data sources themselves.

The central structural rule is inward dependency: Data Source-specific and
externally exposed crates depend on shared inner crates, while the shared
layers do not depend on a particular source or assembled connector.

## Product context

Connectors serve the other ScalarDB Analytics components. Query Engine
adapters live in this repository under `adapters/`, beside the connectors they
call, starting with the [DuckDB adapter](../adapters/duckdb/README.md); the
type conversion library and the Physical Catalog live outside it:

| Component | Unit | Responsibility | Does not hold |
| --- | --- | --- | --- |
| Connector | One per supported system; generic ODBC Connectors can each handle several systems | Metadata retrieval, deciding Analytics types, obtaining current Data Source facts, supplying tested remote/local rewrite rules for offered queries, constructing executable remote queries, and producing values typed by Analytics types | Query Engine native-plan knowledge |
| Adapter | One per Query Engine | Recognizing rewritable native regions, constructing complete alternative Substrait requests, choosing engine-specific join shapes, rejecting results it cannot lower, atomically installing adoptable results, and binding remote output into the Query Engine. Holds the mapping from Analytics types to Query Engine types, with its losses, as a table. | Data source semantics and source-query construction |
| Type conversion library | One | Converting between Analytics types and the schema carried by connector output; shared by the Physical Catalog and adapters | Decisions other than conversion |
| Physical Catalog | One | Persisting the Analytics types and Data Source types connectors returned at registration, and serving them for browsing and planning | Type conversion |
| Query Engine | — | Mapping connector output to its own types and executing queries | Direct access to data sources |

Adding a data source adds no adapter, and adding a Query Engine adds no
connector.

Every column in table metadata carries an Analytics type, or is an unsupported
column, together with its Data Source type. Logical plan construction, catalog
browsing, and cross-node references use those Analytics types, registered in the
Physical Catalog or read from the Connector ([Catalog paths](#catalog-paths)),
without contacting the source again. During connector pushdown, the Connector resolves the
opaque binding and obtains the current source facts needed to decide remote
execution safety. A Query Engine maps Analytics types to its own types through
its adapter. The reasons for this arrangement, including the properties it
guarantees and the alternatives it rejects, are recorded in
[Make one connector interpret each data source](design/make-one-connector-interpret-each-source.md).

A schema appears at three points, each with a distinct role:

| Point | Origin of the schema | Role |
| --- | --- | --- |
| Planning | The Analytics types in the table metadata the Query Engine plans with, through either [catalog path](#catalog-paths) | Plan without contacting the source |
| Read start | The Analytics types the connector derives now | Compared with the planned types by the [schema check](connector-contract.md#schema-check) |
| Read | The values the connector returns with the schema | Consistent, because one component decided the schema and produced the values |

### Catalog paths

A Query Engine reads the catalog it plans with, the namespaces, tables, and
table metadata of a Data Source, through one of two paths:

| Path | Source of table metadata | Access control |
| --- | --- | --- |
| Through the Physical Catalog | The table metadata the Connector returned at registration, which the Physical Catalog persists | ScalarDB Analytics authentication and authorization decide what a user sees |
| Directly from the Connector | The Connector's metadata requests on an open Data Source | Only the Data Source's own privileges for the connection's credentials apply |

Both paths serve the same table metadata, so planning does not depend on the
path: the Physical Catalog holds what the Connector returned, and a direct
read returns it now. In particular, a table's binding in an offered plan is the
name path its table metadata returns, whichever path supplied it
([Read request representation](connector-contract.md#read-request-representation)).

An adapter reads the direct path itself and accepts the other as a
replaceable source of table metadata, so that the component reaching the
Physical Catalog depends on the adapter rather than the reverse. How a product
deployment positions the direct path, which bypasses ScalarDB Analytics access
control, is not decided here.

## System shape

The Rust workspace has the following dependency direction:

~~~text
connector-api
    |
    +--> connector-postgresql --+--> connector-sql ----+
    |                           |                      |
    |                           +--> connector-adbc ---+
    |                                                  v
    +--> connector-mysql ------> connector-sql    connector-sdk
    |                                                  |
    +------------------------------------------------->|
                                                       v
                                                analytics-types
~~~

Query Engine adapters sit outside the Rust workspace, in their Query Engine's
language, and depend only on the C API that `connector-api` exports, as any
other caller does:

~~~text
adapters/duckdb (C++, CMake) --> C API of connector-api
~~~

The [connector contract](connector-contract.md) defines the language-neutral
semantics used across implementation languages. Public bindings and runtime
formats implement that contract without becoming a Data Source-specific layer.

## Design priorities

1. **Language-neutral boundaries.** Connector requests, responses, declarations,
   and extension encodings have semantics independent of the Rust API.
2. **Data Source isolation.** PostgreSQL, MySQL, and future source behavior remains in
   its connector or an explicitly shared access layer.
3. **Inward dependencies.** Shared schema and SDK crates do not depend on
   Connectors or the final C API assembly.
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
| connector-sdk | Data Source-independent behavior and the interface used by Connector implementations |
| connector-adbc | Shared ADBC-based access behavior |
| connector-sql | Behavior shared by SQL-family connectors |
| Connector | One supported system's catalog interpretation, type mappings, and Data Source-specific processing |
| connector-api | C API implementation and assembly of the connector crates |
| DuckDB adapter | DuckDB's catalog of attached Data Sources, scan planning and result adoption, and the Analytics-to-DuckDB type mapping |

An outer component may select and compose capabilities from inner components.
An inner component must not inspect or branch on the identity of an outer
source. Connector crates do not depend on one another. The detailed
responsibilities and dependency rules are in
[Connector SDK](connector-sdk.md#structure-and-dependency-direction).

## Connector flow

A Query Engine adapter recognizes native regions and may construct multiple
complete offered Substrait plans. The SDK validates each request. The Connector
resolves opaque table bindings, obtains the Data Source facts needed by that
region, decomposes the query into a remote plan and optional local plan, and
generates the directly executable remote query. Together these steps are
**pushdown planning**. Each result is a proposal: the Adapter rejects it if the
remote output or complete
local plan cannot be installed, and may consider another request instead.
`PlannedPushdown` crosses the assembled connector API and is delegated to the
Connector for remote execution after the Adapter adopts it.

~~~text
native region + current connector scan
  -> adapter builds one or more PushdownRequests with complete offered plans
  -> connector-sdk validates Substrait and produces a LogicalPlan
  -> Connector loads private metadata for bindings and required source facts
  -> connector-sdk applies the Connector's rewrite rules to a complete QueryPlan
  -> Connector generates the remote executable query from the final remote plan
  -> connector-sdk checks common carriers and returns PlannedPushdown
  -> Adapter lowers the local plan and checks mappings without changing native state
  -> Adapter rejects the proposal or installs it atomically
  -> Connector ships and executes the remote query
  -> Query Engine executes the local plan over its output
  -> connector-sdk validates the Arrow schema and execution lifecycle
~~~

Substrait remains the language-neutral interchange representation. After
validation, the SDK converts the accepted Substrait semantic profile to the
strictly typed `LogicalPlan` used by connector planning. It converts completed
remote and local plans back to Substrait for the connector boundary; it does
not evaluate residual predicates over returned Arrow batches. The detailed
planning model is recorded in [Pushdown planning](pushdown-planning.md), and
the representation decision is recorded in
[Separate interchange and planning representations](design/separate-interchange-and-planning-representations.md).
That document is also the glossary and semantic source of truth for pushdown
planning, query decomposition, remote query generation, and query shipping.

Schema information crosses the source boundary as an Analytics schema. Read
results carry it as their Arrow schema, and metadata carries it as typed C
structures of Analytics types:

~~~text
caller
  -> connector-api
  -> Connector
  -> shared SQL / ADBC / SDK capabilities
  -> data source

data source schema
  -> Data Source-specific mapping
  -> Analytics schema
  -> Analytics type structures in metadata, and the Arrow schema of read results,
     at the connector boundary
~~~

The precise request and response shapes are owned by the
[connector contract](connector-contract.md). The shared type vocabulary and
Arrow conversion rules are owned by the
[Analytics type model](analytics-types.md) and enforced by `analytics-types`.

## Type-system boundary

Analytics types form the source-independent federation schema. Data Source types,
Arrow types, and Query Engine types are concrete boundary representations:

~~~text
Data Source type
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
- Dependencies point from assembly and Connector crates toward shared
  crates, never in the reverse direction.
- Data Source-specific catalog notation and behavior remain outside analytics-types
  and connector-sdk.
- Native driver dependencies belong to independently built driver artifacts,
  not connector-adbc, connector-sdk, or Connector crates.
- Every schema field has one Analytics type; there is no physical-versus-logical
  layer in the federation schema.
- Arrow representation details belong to the Analytics-to-Arrow conversion
  boundary.
- Unsupported source or Arrow representations fail explicitly rather than
  silently changing the Analytics type.
- Only a plan-aware connector rule can authorize a native planner rewrite.
- Planned state contains no credential, open connection, or process-local
  pointer.
- Connector execution returns the remote output schema recorded by
  `PlannedPushdown`; the SDK
  rejects mismatched batches and enforces cancellation and deadlines.
- A column the source declares is never dropped: a column that cannot be mapped
  is returned as an unsupported column with its Data Source type.

## Documentation map

| Document | Read it to |
| --- | --- |
| [Connector contract](connector-contract.md) | Call a connector, or check what a connector must guarantee |
| [Analytics type model](analytics-types.md) | Understand the shared type vocabulary and its Arrow encoding |
| [Pushdown planning](pushdown-planning.md) | Define the pushdown glossary, lifecycle, query decomposition, remote query generation, ownership, and planning boundary |
| [Query-engine pushdown integration](query-engine-pushdown.md) | Map Spark, Trino, DataFusion, or DuckDB planner hooks onto the shared pushdown model |
| [Connector SDK](connector-sdk.md) | Change the connector SDK, the connector API implementation, or crate structure |
| [Connector implementation conventions](connector-implementation.md) | Implement or change one Connector |
| [Query Engine adapter implementation conventions](adapter-implementation.md) | Implement or change one Query Engine adapter |

Callers of connectors (adapters, the Physical Catalog, and the application)
need the contract and the type model. Connector implementers continue with the
type mappings, the connector SDK, and the implementation conventions, in that
order. Adapter implementers continue with pushdown planning, the Query Engine
pushdown mapping, and the adapter implementation conventions.
