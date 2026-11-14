# Connector SDK

## Summary

This document describes the design of the connector SDK (`connector-sdk`,
`connector-adbc`, and `connector-sql`) and of `connector-api`, which assembles
the SDK and the Connectors into the connector API library that satisfies the
[connector contract](connector-contract.md). Read it to change those crates,
or to look up the traits and flows a Connector implementation relies on. It
covers crate responsibilities and
dependency rules, how processing is divided between the connector SDK and
connectors, the validated Substrait planning state, the traits a
connector supplies, the connector API implementation, lifetimes and resource ownership,
concurrency, cancellation and cleanup, ADBC driver handling, and the
credential boundary. Conventions for implementing one Connector
are in [Connector implementation conventions](connector-implementation.md).
The repository-wide boundaries are summarized in [Architecture](architecture.md).

The code shapes below are illustrations of the intended interfaces. Argument
details are decided in the implementation.

## Vocabulary

| Term | Meaning |
| --- | --- |
| Connector | The product-specific implementation that interprets one kind of Data Source. It owns Data Source-specific catalog, type-mapping, planning, and translation behavior. PostgreSQL and MySQL are separate Connectors. |
| Data Source | One configured and opened external system instance. It holds validated connection settings and credentials and creates Data Source Sessions. A Connector is code; a Data Source is a runtime object. |
| Data Source Session | One isolated session opened from a Data Source. With ADBC access it owns one connection; with direct access it owns the equivalent operation-scoped client state. |
| Data Source access | The mechanism that reads a Data Source catalog, runs queries, and receives Arrow. `connector-adbc` provides ADBC access; a direct-access Connector reads through reader crates or a client itself. |
| Table reference | An unresolved non-empty table name path used by metadata requests. It is not a table binding and carries no proof that it belongs to an open Data Source or registered schema. |
| Table binding | A non-secret opaque identity for one table: the name path its table metadata returns. A caller copies it from the table metadata, read from the Connector or from the Physical Catalog, into a Substrait `namedTable`; only the Connector interprets it. |
| SQL dialect | Connector-owned representation rules for SQL identifiers and typed literals. It does not represent a Data Source, connection, table, or common SQL grammar. |
| Connector trait | The SDK's `Connector` trait, the library's entry point for one Connector, together with its component traits and the layer traits (`AdbcConnector`, `SqlConnector`) that supply components. The connector SDK uses a connector only through these traits ([Connector trait](#connector-trait)). |
| Validated plan | A `LogicalPlan` produced by validating and converting an offered Substrait plan in the SDK-supported semantic profile ([Read representation](#read-representation)) |
| Planning metadata | Connector-private, per-call facts used by rewrite rules and executable-query generation; its shape is specific to the Data Source and it does not cross the planning boundary |
| Pushdown planning | Query decomposition followed by remote query generation. It ends when the Connector returns a planned pushdown. See the canonical [pushdown glossary](pushdown-planning.md#glossary). |
| Query decomposition | Semantics-preserving transformation of an offered query into a remote plan and optional local plan; it may rewrite rather than merely split operators |
| Remote query generation | Translation of a remote plan into a Data Source-executable query |
| Planned pushdown | Connector proposal containing a remote plan, optional local plan, and generated executable query |
| Query shipping | Runtime transmission of the generated remote query to the Data Source; it is part of pushdown execution, not pushdown planning |
| Residual operator | A native Query Engine operator retained by its adapter; it is never represented or evaluated by the connector SDK |

Use **Connector** for implementation ownership and **Data Source** for the
external system or its runtime handle. Do not shorten either to “source” when
that would leave the ownership ambiguous. Use a narrower domain term such as
**table binding**, **Data Source type**, or **SQL dialect** when that is the
actual concept.

## Implementation form

Connectors are implemented in Rust. The connector SDK and every connector are
built into one library that exposes the connector API's C ABI and C header.
One library avoids a custom plug-in mechanism for loading separately built
connectors.

JVM and JDBC are kept out of both the library and the ADBC drivers it loads, in
either form of Data Source access. A JVM would have to start inside DuckDB and
DataFusion, which breaks the distribution and performance assumptions of
embedded Query Engines and of the coordinator, and the product does not
require users to provide an external JVM. The only candidate exception is an
ADBC driver for ScalarDB, if ScalarDB uses ADBC access; direct access is built
into the library and cannot be an exception. That exception starts a JVM in
every process that loads the driver, so its impact on these assumptions must
be evaluated before it is adopted.

## Structure and dependency direction

### Crate responsibilities

The connector SDK consists of three crates: `connector-sdk`, `connector-adbc`,
and `connector-sql`.

| Crate | Responsibility | Does not hold |
| --- | --- | --- |
| `analytics-types` | The Rust representation of Analytics types and their conversion to and from Arrow schemas and extension types | Data source knowledge |
| `connector-sdk` | The `Connector` trait, its component traits, and the flows it provides (pushdown planning, planned reads with the schema check and value conversion, and error classification), connector lifecycle, metadata and type-mapping contracts, the comparison of two observations of one table, Substrait validation, complete-request planning contracts, executable-query state, output-schema enforcement, cancellation, deadlines, error classification, and secret-safe errors. Its `runtime` module implements the contract's units and calling conventions for callers: dispatch by Connector name, option validation, unit lifetimes, the session lease and usability, lazy reads, synchronous metadata requests and the cancellations callers pass to them, cancellation, the timeout watcher, and panic containment. Its `conformance` module, behind the feature of that name, is the conformance suite that connector crates run through `runtime` with their fixtures ([Tests](connector-implementation.md#tests)) | The C ABI, ADBC, native Query Engine plans or residual evaluation, and knowledge of any particular Data Source |
| `connector-adbc` | The ADBC access implementation provided by the connector SDK: the `AdbcConnector` trait and the `Adbc<C>` access component built from it (loading drivers through the ADBC driver manager and checking their versions, passing options, opening Data Sources and connecting sessions), running queries and receiving Arrow, cancellation, receiving ADBC errors, and classifying errors by ADBC status and SQL-standard SQLSTATE class where the Connector's error table does not match ([ADBC driver handling](#adbc-driver-handling)) | Deciding types, and knowledge of any particular Data Source |
| `connector-sql` | Processing common to SQL-speaking Data Sources, using `connector-adbc` for Data Source access: the `SqlConnector` trait and the catalog, planner, and executor components built from it, running catalog queries, the filter and projection rewrite rules over the Connector's semantic decisions, generating SQL from remote `LogicalPlan` values with the Connector's dialect, and sending the planned statement | Knowledge of any particular Data Source or native Query Engine plan |
| `crates/connectors/<connector>` | One Connector: its `Connector` implementation and its declarations to the layer traits, written as Rust code and constant tables. An ADBC-access Connector also implements `AdbcConnector`, and a SQL-speaking one also `SqlConnector`; a direct-access Connector supplies its own components and reads the Data Source itself. | Knowledge of other Connectors |
| `connector-api` | The connector API's C ABI and its header, the C structures of metadata and comparisons and their conversion to and from the SDK's types, registering Connectors by feature, the one library a process initializes, and the static information document. It builds the library. | Knowledge of any particular Data Source, and the contract's calling conventions, which it takes from `connector-sdk`'s runtime |
| `adbc-drivers/<connector>` | Our own ADBC drivers, built with `adbc_core` and `adbc_ffi` | Deciding types and interpreting catalogs |

The [connector contract](connector-contract.md) is the semantic source of truth.
Rust crates enforce the implemented behavior, and callers in other languages,
such as adapters and the Physical Catalog, use the typed connector API.

### Dependency rules

~~~text
connector-api --> connectors/<connector> (SQL family) --> connector-sql --> connector-adbc --> connector-sdk --> analytics-types
connector-api --> connectors/<connector> (direct access) ----------------------------------> connector-sdk
connector-api --> connector-sdk
connector-api --> analytics-types   (Analytics types in metadata structures)
connector-adbc --> ADBC driver manager --(loaded at run time)--> ADBC driver
~~~

- The three connector SDK crates do not depend on connector crates.
- `connector-sdk` does not depend on ADBC. The ADBC driver manager appears only
  below `connector-adbc`.
- Connectors do not depend on one another. Processing two connectors need
  belongs in a connector SDK crate.
- Connector crates do not depend on native libraries. Native dependencies stay
  inside ADBC drivers
  ([Isolating native dependencies](connector-implementation.md#isolating-native-dependencies)).
- A direct-access connector depends only on `connector-sdk`, not on
  `connector-adbc` or `connector-sql`.
- A connector that uses ADBC access for a source that does not speak SQL, as
  ScalarDB would with ADBC access, does not depend on `connector-sql` and uses
  `connector-adbc` directly.
- `connector-api` knows nothing of connector contents and calls registered
  connectors only through the connector trait.

### Build cycle and versions

Connectors can be added on a life cycle independent of the rest. This means:

- **Development and testing:** each crate builds and tests on its own. For
  example, the PostgreSQL connector runs the conformance suite with its
  fixture ([Tests](connector-implementation.md#tests)) without building other
  connectors.
- **Addition:** adding a Connector is completed by adding a connector crate,
  the Connector's type mapping, and, with ADBC access, its ADBC driver. The
  connector SDK, the C ABI, and its callers do not change; `connector-api`
  gains one feature and the registry entry behind it, and nothing else in it
  changes
  ([Compatibility and versions](connector-contract.md#compatibility-and-versions)).

Both hold with a single artifact. The main effect of one library is that
delivering a new connector requires a library release, and because the library
ships inside the application distribution, that release is a product release.

Version and assembly rules:

- Built-in connectors use only the public API of the connector SDK crates
  (`connector-sdk`, `connector-adbc`, and `connector-sql`), the same API an
  external connector uses. Version rules for the SDK are decided when external
  connectors are accepted.
- Each connector crate has its own version and raises it whenever its mapping
  or coverage changes
  ([Type mappings and coverage](connector-contract.md#type-mappings-and-coverage)).
  That version is published as the connector's name and version in the
  static information, so retrieval info records which connector version
  produced a result even with a single artifact.
- `connector-api` features select which connectors enter the artifact.
- CI runs the tests of changed crates and the crates depending on them. The
  `connector-api` job assembles the artifact.

Splitting the artifact is reconsidered when connectors must be delivered
without a product release. Splitting needs either a second, versioned C ABI
between `connector-api` and connectors, or a connector SDK copy in every
artifact duplicated within a process; the latter would also require revisiting
the rule that a process loads one library
([Units and concurrency](connector-contract.md#units-and-concurrency)). The
rule that `connector-api` calls connectors only through the trait keeps that
option open.

## Division of processing

The connector SDK and connectors divide the elements of the external contract
as follows:

| Contract element | Connector SDK | Connector |
| --- | --- | --- |
| Metadata retrieval | The table metadata model and its states, the deterministic order of columns and constraints, deriving completeness, comparing a stored result with a newer retrieval, cancellation and deadlines of catalog reads, and the retrieval info model | Catalog queries and reading, the source metadata type, the type mapping and unsupported reasons, how constraints and identifiers are read, and rules for detecting missing information |
| Data read | Offered Substrait validation and `LogicalPlan` conversion, initial `QueryPlan` construction, application of the Connector-supplied rule set, decomposition-contract assertions, cancellation, and deadlines | Obtaining current Data Source/session facts, supplying remote/local rewrite rules, generating and executing remote queries, guaranteeing rewrite semantics, and converting source output |
| Calling conventions | Cancellation and timeouts, the error structure, and credential handling; with ADBC access, loading and version-checking the ADBC driver (`connector-adbc`) | Mapping connection options to driver options, fixed settings, and classifying Data Source-specific error codes; with direct access, reading the Data Source |

The rule is that whatever must apply with the same meaning to every connector
belongs in the connector SDK, and whatever needs source knowledge belongs in the
connector. The SDK reaches a connector only through the `Connector` trait and
its components (`Access`, `Catalog`, `Planner`, `Executor`, and
`ErrorHandler`; [Connector trait](#connector-trait)), and runs the shared
planning and read flows itself, so common validation, planned-state checks,
the schema check, and the table metadata model have the same SDK boundary in
both forms of Data Source access. The type mapping needs source knowledge, so the
connector applies it; the SDK fixes only the types that carry its result. With ADBC access, connection, query
execution, Arrow results, cancellation, and error retrieval are also the same
`connector-adbc` processing for every source, leaving only source knowledge to
the connector. With direct access, the connector reads the Data Source itself
behind the same traits.

## Read representation

The connector SDK validates the received Substrait plan's expected shape and
converts the accepted semantics to `LogicalPlan`, the strictly typed internal
representation used during connector planning. `LogicalPlan` is not a second
wire format or an independent query language: it represents the SDK-supported
semantic subset of Substrait without the structurally invalid states permitted
by the generated protobuf model. Its `Read`, `Filter`, `Project`, and
`RemoteResult` relation variants make required children and conditions
non-optional. `LogicalExpression` is a recursive connector-owned model whose
nodes retain their Analytics type, nullability, resolved operation, and
operands. Known unary and binary operators carry operands in arity-specific
nodes rather than an unchecked argument list. This lets planning move a
subexpression independently during partial pushdown. Boundary conversion
regenerates Substrait expressions from that model.
Schemas, extension declarations, and version metadata likewise retain their
Substrait payloads for lossless boundary conversion. Dynamic-
parameter expressions and bindings are rejected at that boundary. Planning
rules transform trees, so `LogicalPlan` is
immutable through its public API. A rule chooses the next relation tree; the
`LogicalPlan` construction operation, rather than the rule, owns maintaining
plan-level derived state and stores the corresponding output schema.

~~~text
RemoteExecution<ConnectorQuery> {
    query: ConnectorQuery (PortableQuery)
    output_schema: Analytics schema
}

PlannedPushdown {
    remote_plan: Substrait Plan
    local_plan: optional Substrait Plan
    remote: RemoteExecution<ConnectorQuery>
    base_schema: Analytics schema
    final_output_schema: offered result schema
    table_bindings: opaque identifiers from NamedTable
}
~~~

`LogicalPlan` is the successful result of validating and converting the
offered Substrait plan, and is the direct input to Connector planning.
`PlannedPushdown` is the Connector's direct planning result and proposal to the
Adapter. Its design is described in
[Pushdown planning](pushdown-planning.md).

Validation forms the conversion boundary. For every Substrait plan accepted by
the current validation profile, converting to `LogicalPlan` and back preserves
the query meaning and metadata required by that profile. The guarantee is
semantic rather than byte-level: protobuf serialization details and unknown
fields are not part of the internal planning contract. Completed remote and
local `LogicalPlan` values are converted back to Substrait when constructing
`PlannedPushdown`.

The adapter supplies one semantically complete offered plan, without a separate
Query Engine/session context. After common validation, the Connector resolves
the offered plan's opaque bindings and obtains whichever catalog or source
facts that complete region requires. Resolution may perform Data Source I/O. A
session-local cache or prefetch step is optional and does not change the
contract. This prevents an adapter from asserting that an old catalog value is
the source's current collation, version, or snapshot.

The resulting planning metadata is a Connector-private type. Its shape may
differ by Data Source and may include physical identifiers, source types,
collations, capabilities, or any other facts needed by that Connector. It is
created for one planning call, borrowed by rewrite rules and executable-query
generation, and then dropped. The SDK sees it only as the `Planner`'s opaque
`Facts` type; it does not appear in `PlannedPushdown` or the language-neutral
connector API.

Source attributes that planning relied on, such as collations, character sets,
the Data Source version, and session settings, are treated as unchanged when
the planned query runs; the result is not guaranteed if they change
([Read request](connector-contract.md#read-request)). Execution checks only the
schema ([Schema check](connector-contract.md#schema-check)).

While building the representation, the connector SDK checks the plan and
rejects it if a check fails:

| Check | Error on failure |
| --- | --- |
| The plan fits the shared request shape | `invalid request` |
| Fields required to derive the table binding and input/output schemas are present and structurally usable | `invalid request` |
| Filter field references are direct base-schema positions within range | `invalid request` |
| Referenced scalar-function anchors are declared, their URI anchors resolve, and the initial comparison, Boolean, and null-test signatures have the required arity and Boolean result | `invalid request` |

Passing these checks does not prove that the remote/local rewrite preserves the
offered semantics. Each Connector rewrite rule owns that guarantee and may
return `Unsupported` without turning a normal optimizer attempt into an error.

## Connector trait

The library's entry point for one Connector is the SDK's `Connector` trait. A
Connector selects the components it is built from through associated types and
declares what only it knows. The provided functions of `Connector` are the
flows that apply with the same meaning to every Connector, so a Connector does
not implement them:

~~~text
Connector:
    type Access: Access<Self>           how the Data Source is reached
    type Catalog: Catalog<Self>         how metadata requests are answered
    type Planner: Planner<Self>         how offered plans are decomposed and the
                                        query generated
    type Executor: Executor<Self>       how a generated query is sent
    type ErrorHandler: ErrorHandler     the error table and redaction, over the
                                        access's diagnostic type
    type Query: PortableQuery           the Data Source query planned and run

    descriptor() -> ConnectorDescriptor            (available before initialization)
    convert_column(column, target Analytics type) -> column

    provided:
    plan(session, request) -> Planned(PlannedPushdown) | Unsupported
    read(session, RemoteExecution<PlannedRead<Query>>) -> ExecutionOutput
    classify(diagnostic) -> ConnectorError
~~~

The component traits are:

~~~text
Access<C>:
    type Library, DataSource, Session     the state the SDK's units hold
    type Diagnostic                       what the access reports about an error
    description() -> DataSourceAccess     (for the descriptor)
    open_library(library_settings) -> Library
    open_data_source(library, connector, connection_options, connection_timeout)
        -> DataSource
    open_session(data_source) -> Session
    component(session) -> optional driver Component     (for retrieval info)
    error(diagnostic, optional ErrorClass) -> ConnectorError

Catalog<C>:
    type Table: SourceTableMetadata
    type Context                          session-level facts, read once when a
                                          session opens
    open_context(access session) -> Context
    namespace_list(session, filter) -> ExecutionOutput<Listed<NamespacePath>>
    table_list(session, namespaces, tables) -> ExecutionOutput<Listed<TableEntry>>
    table_metadata(session, tables) -> ExecutionOutput<TableRecord<Table>>
    provided:
    compare_source_metadata(before Table, after Table)
        -> no difference | SourceMetadataDifferences

Planner<C>:
    type Facts                            Connector-private planning metadata
    planning_facts(session, offered_plan) -> (TableReference, Facts)
    rewrite_rules() -> RewriteRuleSet<Facts>       (built once per session)
    generate_query(remote_plan, facts) -> Query | Unsupported

Executor<C>:
    run(session, query) -> ExecutionOutput { stream, ExecutionCancel }

ErrorHandler:
    type Diagnostic
    classify(diagnostic) -> optional ErrorClass    (default: no match)
    redact(diagnostic) -> diagnostic               (default: unchanged)
~~~

The SDK owns the units that hold component state. `DataSource<C>` holds the
access's Data Source state. `Session<C>` holds the access's session state
(`Session::access`), the catalog's context (`Session::context`), and the
planner's rewrite rules; `Session::open` builds it from a newly opened access
session, reading the context on it. Components receive the `Session<C>`, so no
connection or catalog handle is passed separately. The library's per-Connector
state, which holds the access's library state, is private to the SDK.

The provided flows are:

~~~text
Connector::plan(session, request):
    offered_plan = validated LogicalPlan of request.offered_plan
    (table, facts) = Planner::planning_facts(session, offered_plan)
    query_plan = initial complete remote/local QueryPlan
    for rule in the session's rewrite rules in configured order:
        query_plan = rule.rewrite(query_plan, facts)
        unchanged query_plan -> no-op and continue
        error -> stop with error
    a local plan that only returns the remote result is dropped
    query = Planner::generate_query(query_plan.remote_plan, facts)
    unsupported -> return Unsupported without constructing a pushdown
    read = PlannedRead { table, registered base schema of the remote read, query }
    convert the remote/local LogicalPlans to Substrait
    return PlannedPushdown whose RemoteExecution carries read

Connector::read(session, remote):      nothing is sent before the first pull
    Catalog::table_metadata(session, [remote.query.table])
        check_schema(registered base schema, current table metadata)
        a mismatch or a missing table is stale registration; the query is
            never sent
    then Executor::run(session, remote.query.query)
        each batch is converted with Connector::convert_column into the
            planned output schema
    one ExecutionCancel stops the running step and every later one

Connector::classify(diagnostic):
    diagnostic = ErrorHandler::redact(diagnostic)
    class = ErrorHandler::classify(diagnostic)
    return Access::error(diagnostic, class)
        a class from the Connector's error table takes precedence over the
            access's generic classes

Connector SDK execute_planned(output_schema, deadline, start):
    output = start()            the runtime passes Connector::read on the session
    return ExecutionStream, which hands out ExecutionCancelHandle
        cancel() / expire() records cancellation / timeout, first wins,
            then calls the connector's ExecutionCancel
        a pull after a recorded stop returns that stop's error
~~~

`PlannedRead<Q>` is the query of every planned read: the table as metadata
requests name it, the registered schema of the base columns the plan reads,
and the Connector's query `Q`. It implements `PortableQuery` by framing the
table and base schema around `Q`'s own bytes, so a Connector encodes only its
query.

Shared layers supply components for the Connectors that declare what those
layers need. Each layer is a subtrait of `Connector` that pins the components
it supplies through associated-type equality, so that a Connector declares
only what is specific to its Data Source:

~~~text
connector-sdk    Connector, and the component traits Access, Catalog, Planner,
                 Executor, and ErrorHandler
connector-adbc   AdbcConnector: Connector<Access = Adbc<Self>>
                     DRIVER: the ADBC driver and the versions it was tested with
                     database_options(connection_options, connection_timeout)
                 Adbc<C> implements Access<C>
connector-sql    SqlConnector: AdbcConnector + SqlDialect
                     + Connector<Catalog = SqlCatalog<Self>,
                                 Planner = SqlPlanner<Self>,
                                 Executor = SqlExecutor<Self>, Query = SqlRead>
                     Table, Context, PlanningFacts
                     session_context, namespace_list, table_list, table_metadata:
                         each a function that runs its statements on
                         CatalogReads and uses each result where it is fetched
                     planning_facts, runs_remotely
                 SqlCatalog<C>, SqlPlanner<C>, and SqlExecutor<C> implement
                 Catalog<C>, Planner<C>, and Executor<C>
~~~

A SQL-speaking Connector such as PostgreSQL therefore implements three traits:
`Connector`, where it names `Adbc<Self>`, the three SQL components, `SqlRead`,
and its own error handler, and declares its descriptor and value conversion;
`AdbcConnector`; and `SqlConnector`. The component choices in its `Connector`
implementation are the ones the layer traits require, and the compiler rejects
any other. A direct-access Connector implements `Connector` with components of
its own, including an `Access` that reads the Data Source itself. A Connector
with no error table or redaction of its own uses `GenericErrorHandler<D>` over
its access's diagnostic type.

The layers pin components instead of implementing `Connector` for their
Connectors because Rust leaves no other way to share them:

- A subtrait cannot implement its supertrait's items. `AdbcConnector` cannot
  supply `Connector::Access`; it can only require that it be `Adbc<Self>`.
- A blanket `impl<C: AdbcConnector> Connector for C` cannot be written in
  `connector-adbc`: `Connector` is foreign to that crate and `C` is not
  covered by a local type, so the orphan rule rejects it. Blanket
  implementations for different forms of access would also overlap, because
  coherence cannot rule out a type that implements both layer traits.

The components, by contrast, are generic types local to each layer, such as
`Adbc<C>` and `SqlCatalog<C>`, so each layer implements the SDK's component
traits for them.

~~~text
ADBC access (Adbc<C>, from AdbcConnector C):
    open_library keeps the library settings; no driver is loaded yet
    open_data_source loads C::DRIVER through its manifest once per library (a
        failed load is not kept), and opens one ADBC database (AdbcDatabase<C>)
        with C::database_options
    open_session connects, checks the driver version on the first connection,
        and returns AdbcSession<C>: the connection and the driver component
    every driver error becomes an AdbcDiagnostic and goes through
        Connector::classify; Adbc<C>::error applies the ADBC status and
        SQLSTATE classes unless the Connector's error table matched

SQL components (from SqlConnector C):
    SqlCatalog<C>: opening a session runs C::session_context; each metadata
        request runs C's function as one source read on the session's
        connection, at the first pull; CatalogReads::query runs one statement
        to its end, and a cancel stops the running statement and keeps later
        ones from starting
    SqlPlanner<C>:
        planning_facts: C::planning_facts resolves the binding and reads facts
        rewrite rules: the whole filter moves remotely when C::runs_remotely
            accepts its condition; a projection of plain columns moves once no
            local filter remains below it
        generate_query translates the remote plan
            common visitor owns Read/Project/Filter and fixed SQL syntax
            LogicalExpression exposes resolved functions, fields, and validated LiteralValue nodes
            C's SqlDialect renders identifier and LiteralValue fragments
            Fragment composes them into one RenderedQuery
            SqlRead keeps only the SQL; the PlannedRead around it carries the
                table and the registered base schema
    SqlExecutor<C>: passes the SqlRead's SQL to connector-adbc::query on the
        session's connection
        connector-adbc owns lazy statement creation, execution, streaming,
            cancellation, diagnostics, and resource release
~~~

`ConnectorPlanner`, its rule-based form `RewriteRuleConnectorPlanner`,
`PushdownRequest`, and `RewriteRuleSet` remain in the SDK as planning building
blocks. `RewriteRuleConnectorPlanner` takes a metadata loader, ordered rules,
and an executable-query generator, and its blanket `ConnectorPlanner`
implementation runs the same rule loop as `Connector::plan`. The `Connector`
flow itself drives a `Planner` component.

Connector planning runs after the SDK has produced a `LogicalPlan`, because
that is the first point at which the Connector can know which source facts are
needed. It may query the Data Source under the planning operation's ordinary
timeout, cancellation, error, and session-lease rules. Implementations may
reuse cached facts across requests, but no planning-snapshot identity is
required. Only the `RemoteExecution` crosses to another process
([Execution in another process](pushdown-planning.md#execution-in-another-process)).

Through its `Planner`, the Connector provides its private facts loader, ordered
rules, and query generator, while `Connector::plan` owns the common control
flow. The flow returns `PlannedPushdown` directly; no resolved-plan carrier or
second planning result crosses the boundary. The SDK
constructs `LogicalPlan` as part of offered Substrait validation and asserts the
decomposition contract when constructing `PlannedPushdown`. It does not
revalidate Connector-produced plans as external input, interpret Connector
metadata, or prove semantic equivalence.
The Adapter treats the result as a proposal, adopts it only if it can lower the
complete local plan and map the remote output, and otherwise leaves native
state unchanged. The executor directly executes the adopted remote query
without planning or translating it again.

The initial complete decomposition keeps relational processing local: its
remote plan performs the minimum Data Source read, and its local logical plan
contains the remaining `RemoteResult -> Filter -> Project` work. Each rule receives
and returns a complete decomposition. A rule that makes no change is a no-op;
there is no distinct fully-local planning result. `RemoteResult` is a reserved
Substrait extension leaf carrying the remote output schema. The SDK asserts
that the local plan terminates in that leaf rather than an independent read,
that its schema equals the remote output, and that the final output equals the
offered output. These are internal contract assertions, not input validation or
a proof that an arbitrary rewrite preserves query semantics.

The pseudocode describes responsibilities, not required Rust method names or
whether translation and execution are separate implementation calls. The
public connector API remains the language-neutral boundary; these internal
interfaces keep source knowledge out of the SDK.

For SQL-family Connectors, `connector-sql` traverses the validated remote
`LogicalPlan` and its `LogicalExpression` nodes. It does
not interpret Substrait anchors or extension declarations. `planning_facts` creates one
Connector-private metadata value containing the physical names and Data Source
facts needed by the planning call. Rewrite rules and executable-query generation
borrow that same value; no separate SQL metadata model is constructed.

The common visitor owns explicit projection and aliases, the single-table
`SELECT`/`FROM`/optional `WHERE` shape, Boolean composition, and null tests. It
emits the profile's fixed SQL syntax directly; it does not mirror that syntax
in a second token model. `LogicalPlan` owns literal validation and
canonicalization and stores a `LiteralValue` retaining typed null and
logical-carrier information in each literal expression. Connector
rewrites are revalidated when they construct a replacement plan root.
The Connector's `SqlDialect` owns actual representation differences such as identifier quoting
and literal spelling and escaping. The result is one complete
non-parameterized SQL statement. Connector rewrite rules use the concrete Data Source type,
nullability, collation, version, and session facts in the shared metadata before
leaving an expression in the remote plan. SQL translation renders that approved
plan without repeating the semantic decision. The resulting
`RemoteExecution<PlannedRead<SqlRead>>` retains that SQL-specific type through
adoption. Execution sends its SQL text directly; it does not receive or
reconstruct a `LogicalPlan`, planning metadata, dialect, or semantic policy.

Data Source access sits below these traits. It reads catalogs and executes
Data Source queries returning Arrow streams; it does not expose native Query
Engine plans. `connector-adbc` provides the common ADBC access, while a
direct-access connector reads the Data Source itself inside its own
components. Cancellation remains scoped to one execution so another thread
can stop a blocked source call.

`connector-adbc` wraps ADBC in its own types instead of exposing `adbc_core`'s
traits to connectors because:

- `adbc_core`'s traits have associated types and generic arguments and cannot
  be used as trait objects, while `connector-sdk` must hold each connector's
  different implementation behind one type;
- `adbc_core`'s traits have no `Send` and `Sync` bounds;
- `adbc_core`'s cancellation takes `&mut self`, so a running statement cannot
  be cancelled from another thread; and
- `adbc_core` has not reached 1.0 and releases incompatible versions every few
  months, while wrapping it confines those changes to `connector-adbc`.

A Connector's `Catalog` component has one function per metadata request
([Capabilities](connector-contract.md#capabilities)). Table metadata has two
layers: the connector's own source metadata type, which the Semantic Catalog
receives as JSON, and the source-independent table metadata derived from it
([Table metadata](connector-contract.md#table-metadata)).

~~~rust
pub trait Catalog<C: Connector>: 'static {
    /// Connector-specific metadata for one table, such as `pg_catalog` facts.
    type Table: SourceTableMetadata;
    /// Session-level facts read once when a session opens.
    type Context: Send + Sync + 'static;

    fn open_context(session: &AccessSession<C>) -> Result<Self::Context, ConnectorError>;

    /// Namespace list. `filter` is a set of names matched exactly, without case folding;
    /// filter at the source if possible, otherwise after listing.
    fn namespace_list(session: &Session<C>, filter: &NameFilter)
        -> Result<ExecutionOutput<Listed<NamespacePath>>, ConnectorError>;

    /// Table list across namespaces, with a namespace filter and a table-name filter.
    fn table_list(session: &Session<C>, namespaces: &NameFilter, tables: &NameFilter)
        -> Result<ExecutionOutput<Listed<TableEntry>>, ConnectorError>;

    /// One record per requested table.
    fn table_metadata(session: &Session<C>, tables: &[TableReference])
        -> Result<ExecutionOutput<TableRecord<Self::Table>>, ConnectorError>;

    /// Provided: compares two observations of one table's source metadata.
    fn compare_source_metadata(before: &Self::Table, after: &Self::Table)
        -> Result<Option<SourceMetadataDifferences>, ConnectorError>;
}

/// Serialized as the source metadata JSON document; the connector decides its
/// shape. A stored document is restored to this type to compare it.
pub trait SourceTableMetadata:
    serde::Serialize + serde::de::DeserializeOwned + Send + 'static
{
    /// The connector applies its type mapping here.
    fn to_table_metadata(&self) -> TableMetadata;
}

pub enum TableRecord<T> {
    Read { metadata: T, read_at: SystemTime },
    NotFound { table: TableReference },
    Unavailable { table: TableReference, reason: String },
}

/// Built with TableMetadata::new, which orders columns by ordinal and
/// constraints by kind, name, and columns.
pub struct TableMetadata {
    table: TableReference,
    kind: TableKind,                       // Table | View | External | Other(String)
    source_id: Reported<String>,
    columns: Vec<Column>,                  // name, ordinal, nullable, source_id, ty
    columns_may_be_hidden: bool,
    constraints: Reported<Vec<Constraint>>,
}

pub enum ColumnType {
    Mapped { data_source_type: String, analytics_type: AnalyticsType },
    Unsupported { data_source_type: String, reason: UnsupportedReason },
    Unread { reason: String },
}

pub enum Reported<T> { Value(T), Missing { reason: String }, NotOffered }
~~~

Each function returns the read's item stream together with its
`ExecutionCancel`, the same shape as query execution. Callers start it through
`start_source_read`, which applies cancellation and the deadline exactly as
for execution ([Cancellation and timeouts](#cancellation-and-timeouts)). Even
when one catalog read runs several executions at the source, such as one query
per namespace, its `ExecutionCancel` stops the execution in progress. Each
function receives the SDK's `Session<C>`, which carries the access's session
state and the context the catalog read when the session opened.

The three `TableRecord` states correspond one to one to the contract's record
states: `Read` is complete or incomplete, `NotFound` is "does not exist", and
`Unavailable` is "could not be read"
([Missing information](connector-contract.md#missing-information)). Whether
read table metadata is complete is derived from its values
(`TableMetadata::is_complete`): any `Missing` item, any `Unread` column type, or
`columns_may_be_hidden` makes it incomplete, while `NotOffered` does not. An
Analytics type cannot be stated without the Data Source type it was mapped
from.

Source metadata and table metadata hold only what the source states, so equal
source state gives equal values. The retrieval time is retrieval info, not
content, and travels beside them in `TableRecord::Read`.

Comparing two observations of one table follows the same split
([Comparing table metadata](connector-contract.md#comparing-table-metadata)).
Both comparisons return `None` when nothing differs, and otherwise only
values observed on both sides that differ. Table metadata has one shape for
every connector, so the SDK compares it without any connector:
`TableMetadata::compare` returns `TableMetadataDifferences`, whose columns
and constraints are each a `Change` (added, removed, or matched with the
attributes that differ). Source metadata has the connector's shape, so its
comparison belongs to the component that defines that shape:
`Catalog::compare_source_metadata` takes two values of `Catalog::Table`,
compares them as JSON documents, and compares the table metadata derived from
both. Every catalog shares this provided function. A table metadata result
carries each table's source metadata as `SourceMetadata`, its connector's name
and its JSON document. The runtime's `Library::compare_source_metadata` finds
the built-in connector of that name and restores both documents to its
`Catalog::Table`; documents of different connectors, an unknown connector, or
a document that cannot be restored is `invalid request`.

Retrieval info is assembled from each table's `read_at`, the connector's name
and version in its `ConnectorDescriptor`, and the driver component the
access reports for the session through `Access::component` (with ADBC access,
the ADBC driver's name and version). The runtime adds the library version and
returns `TableMetadataResult`: the records, each read table as `TableRead`
with its table metadata and source metadata, and the retrieval info.
`connector-api` converts it to C structures.

## Connector behavior as code

A connector's behavior is written as Rust code and constant tables in its
crate; there is no separate definition file. Data Source access settings, the
catalog queries, type correspondence, the SQL dialect, the semantic conditions
of rewrite rules, and the classification of Data Source-specific error codes
are all part of the connector traits' implementations. Items that appear in
[static information](connector-contract.md#static-information), such as the
connector's name and version, its accepted connection options and which of
them are secret, and, with ADBC access, the driver name and supported version
range, are returned as Rust values through the connector traits.

Correspondence that is naturally a table, such as dialect translation tables
and type correspondence, is written as constant tables or a `match` over the
source's values, so it can be checked
against the connector's type mapping and conformance expectations.

## Connector API implementation

The [connector API functions](connector-contract.md#functions) are
implemented in two layers. `connector-sdk`'s `runtime` module implements the
contract's units and calling conventions in Rust, and `connector-api` exposes
them through the C ABI.

`connector-api` owns what is specific to the C library:

- **Registration:** connectors in the artifact are registered at build time
  and selected by features.
- **The process-wide library:** one library is initialized per process; a
  second initialization returns `invalid state`.
- **Static information:** assembled from the registered connectors'
  `ConnectorDescriptor` values.
- **The C representation:** handles, error structures, text conventions, the
  Arrow C stream of read results, and the C structures of metadata and
  comparisons with their conversion to and from the SDK's types.

The runtime owns the contract's semantics:

- **Dispatch:** opening a data source selects the connector for the named
  Connector. That connector handles all sessions and operations on that data
  source.
- **Option validation:** the caller's options are checked against the
  selected connector's descriptor before the connector sees them. An
  undeclared, repeated, or missing required option, or a value of the wrong
  form, is `invalid settings` naming the option. Every value is held in an
  erasing secret type while it is checked, and the values of options marked
  secret stay in that type.
- **Unit management:** the library, data sources, sessions, read operations,
  and result streams are managed as described in [Lifetimes](#lifetimes).
- **Metadata requests:** `SessionState::namespace_list`, `table_list`, and
  `table_metadata` are synchronous calls. Each takes the session lease for
  the call, reads the catalog to its end, and returns the whole result or only
  an error. The timeout counts from the call, and a caller's `Cancellation`
  stops the catalog read from any thread. A connector's source metadata type
  is erased here: the runtime serializes it as the JSON document, derives the
  table metadata from it, and adds retrieval info from the data source's
  connector, the session's ADBC driver, and the library version.
- **Source metadata comparison:** `Library::compare_source_metadata`
  dispatches by the connector name the source metadata carries, restores both
  documents to that connector's `Catalog::Table`, the only place that type is
  known behind the object-safe view, and calls
  `Catalog::compare_source_metadata`.
- **Planning errors:** a Connector's planning failure carries a
  `ConnectorError`, so `plan_pushdown` returns its kind, retryability, and
  Data Source details unchanged.

A Connector's units carry types that differ per connector. A library
registers each Connector with `BuiltInConnector::of::<C>()`, and the runtime
implements one object-safe counterpart of each unit for every `Connector` and
holds connectors only through those, so dispatch by Connector name needs no
knowledge of a particular connector. A remote
execution keeps its connector's query type behind the same boundary; a read
of a remote execution planned by another connector is `invalid request`.

The C functions and types are declared in `connector-api`'s `src/abi`, the
only module that handles raw pointers. The published header is generated from it with
cbindgen, and a test fails when the committed header differs from the
generated one. The library exports only
`scalardb_analytics_connector_api`, which returns a table of the other
functions, so the header names them without a prefix and only the one symbol
must be unique in the process. Every C function contains panics; a function that fails returns
an error the caller frees, and an operation's error is owned by the operation.

The C structures of metadata mirror the SDK's types but have a different
role. The SDK's types are validated domain values and know nothing of C; the
dependency runs one way, from `connector-api` to `connector-sdk`. The C
structures are non-owning memory layouts:

- **Output:** the library converts an SDK value into a tree of C views in
  the shape of an ordinary C API and passes it to the caller, which then owns
  the duty to free it. Every allocation the views point to is kept in an
  arena behind the root view, invisible to C, so the root's free function
  releases the whole tree at once instead of walking it.
- **Input:** the library reads the caller's structures during the call,
  checks them, and builds SDK values; anything not valid, including an
  enumerated value outside its constants, is `invalid request`. Enumerated
  fields and truth values are integer newtypes with named constants rather
  than Rust enums and `bool`, because an input may hold any bit pattern.

Because output text and input text use the same string structure, a
retrieved record's table metadata or source metadata is passed back to a
comparison unchanged. A unit test writes SDK values covering every variant
of the mirrored enums to C structures and reads them back, asserting that they
equal the originals, so drift between the two sets of types fails a test.

## Lifetimes

| Unit | Created | Holds | Released |
| --- | --- | --- | --- |
| Library | At initialization | Settings, each registered Connector's access library state (with ADBC access, the ADBC driver, loaded when the first Data Source opens), and the timeout watcher thread | Closed at shutdown; its state, including the watcher, lives until its last Data Source, session, and operation are released, so running reads still stop at their deadlines. Loaded ADBC driver shared libraries stay loaded until the process exits ([Resource ownership](#resource-ownership)). |
| Data source | When opened | The selected connector, validated connection settings and credentials, and, with ADBC access, its ADBC database | When the data source and every session using it are closed |
| Session | When opened | Its `Session<C>`: the access's session state (with ADBC access, an ADBC connection and the driver component), the catalog's context read when it opened, and the planner's rewrite rules | When closed, releasing its `Session<C>` |
| Operation | When a read starts: validates the request and takes a session lease | Data Source resources: the session lease and, after the first pull, its read state and `CancelHandle`; also cancellation, deadline, and error state | When closed, releasing every Data Source resource. Read resources are also released, and the lease returned, when the stream ends or is released. |
| Result stream | When a read starts | A reference to the operation, conversion state, expected output schema, and cancellation/deadline state. It holds no Data Source resources. | On the stream's `release`; the operation stops the Data Source query and releases the stream returned by Data Source access |
| Cancellation | When the caller creates it | Whether it was cancelled, and the cancel handles of the catalog reads running with it | When the caller frees it, after no request runs with it |
| Metadata result, comparison differences | When a metadata request or a comparison returns | The C views and the arena they point into; no Data Source resources | When the caller frees the root with its free function |

The caller-visible rules (one running operation per session, no pool, release
order) are in [Units and concurrency](connector-contract.md#units-and-concurrency).
They are realized as follows:

- Internal state is shared by reference counting. If a parent is closed first,
  its internal state survives while a child refers to it, so freed memory is
  never touched.
- Each Data Source access unit is released when the last reference to its owner
  disappears. With ADBC access, `connector-adbc` keeps ADBC's release order
  (children first).
- A call on a child after its parent was closed sees the parent's closed flag
  and returns `invalid state`.

## Resource ownership

| Resource | Owner | Rule |
| --- | --- | --- |
| Internal state of connector API units | The library | Released by the matching close function |
| `ArrowArrayStream` | The caller that received it | The caller calls the stream's `release`; the library then releases the stream returned by Data Source access and stops the query |
| `ArrowArray` and `ArrowSchema` | The caller that received them | Each array is released individually, per the Arrow C data interface. Unconverted columns pass on the buffers Data Source access returned; converted columns pass buffers the library created. |
| Strings, bytes, and structures the caller passes (option values, Substrait plans, table names, table metadata, source metadata) | The caller | The library reads or copies them during the call and does not refer to them afterwards |
| Metadata results and comparison differences | The caller that received them | The library allocates the tree and passes it to the caller, which frees it once with the free function of its root; that releases the whole tree, and no pointer into it stays valid afterwards |
| Error structures | The library | Valid until the caller calls the release function; an operation's error is valid until the operation is closed |
| Credentials | The data source | [Credential boundary](#credential-boundary) |

A stream pulls one batch at a time from the stream returned by Data Source access,
converts it, checks the planned schema, and returns it. The connector SDK does not evaluate residual expressions or read
ahead, so a read's memory is bounded by the Data Source access batch and the
converted batch.

With ADBC access, unconverted columns pass the driver's buffers on directly.
Unloading the driver's shared library while a caller still holds such an array
would make the array's `release` call unloaded code, so the library keeps
loaded ADBC drivers until the process exits. Direct-access reader crates are
built into the library and have no such problem.

## Concurrency, cancellation, timeouts, and cleanup

### Execution model

Data Source access is synchronous. ADBC is a synchronous C API, and the
connector SDK has no asynchronous runtime. Starting an operation only validates
the request (including the checks in [Read representation](#read-representation))
and does not call Data Source access. A read's Data Source access is called
from the caller's thread inside a stream pull, and a metadata request's inside
the call itself; planning resolution may also call it from `plan_pushdown`. At
a read's first pull, the connector SDK translates the request, starts
`Connector::read`, stores the returned execution and its cancel handle in the
operation, and then pulls its stream. A metadata request starts the catalog
read, registers its cancel handle with the timeout watcher and with the
caller's cancellation, and pulls it to the end before returning. A direct-access
implementation that uses an asynchronous reader crate keeps that runtime inside
the implementation and waits for results within the component call; the
runtime never leaves the component.

`AdbcQueryStream` is the state machine for one lazy ADBC query. Its
`QueryState` enum encodes the current lifecycle phase. The stream and its cloneable
`AdbcQueryCancelHandle` share an `Arc<Mutex<QueryState>>`: `Arc` keeps the query
state available until the stream and all cancellation handles have been dropped,
and `Mutex` serializes state transitions from stream reads, cross-thread
`Cancel` events, and cleanup. The stream owns one `Arc` reference, and each
cancellation-handle clone owns another reference to the same state. The stream
locks the state when moving resources into a blocking read and when recording
its result. A cancellation handle locks it to inspect the phase and record the
cancellation transition. Stream drop locks it to transition to `Finished`, then
drops the replaced state after releasing the lock.

The query states are `Pending`, `Preparing`, `Ready`, `Reading`, `Cancelled`,
and `Finished`. `Preparing` distinguishes statement creation from reader
creation because only the latter has a statement-scoped cancel handle.
`Pending` and `Ready` own query resources between reads. During the blocking
`Preparing` and `Reading` operations, the polling thread temporarily owns those
resources while the shared state records the in-progress phase and cancellation
capability. This ownership tradeoff is recorded in
[Transfer ADBC query resources to blocking polls](design/transfer-adbc-query-resources-to-blocking-polls.md).
The state mutex is not held during ordinary source calls. It protects the
`QueryState`, while the polling thread exclusively accesses any request,
statement, or execution transferred out for the current blocking call. Native
cancellation is the exception: the cancellation handle keeps the state mutex
locked while invoking the statement-scoped callback so cleanup cannot release
the statement concurrently.

### Thread safety

| Unit | Concurrent calls |
| --- | --- |
| Library, data source | Allowed from several threads |
| Session, operation, result stream | One at a time ([Units and concurrency](connector-contract.md#units-and-concurrency)), except that an operation may be cancelled from another thread |
| Cancellation | Allowed from several threads; it is cancelled from another thread while a metadata request runs with it |

A data source is used from several threads. The access's Data Source state
(`Access::DataSource`) is `Send + Sync`, and a direct-access implementation
must behave correctly when sessions are opened concurrently. ADBC does not require drivers to support
concurrent use of a database, so `connector-adbc` serializes database
functions (creating connections). The `Connector` trait is also `Send + Sync`.

### Cancellation and timeouts

Cancellation propagates in this order:

1. The caller cancels a read operation, or the cancellation a metadata
   request was given, from any thread.
2. The caller's `ExecutionCancelHandle` records cancellation and calls the
   `ExecutionCancel` the connector returned with that execution
   (`ExecutionOutput`).
3. The implementation stops the work for its form of Data Source access:
   - **ADBC access:** at the first pull, `connector-adbc` creates one statement
     for the execution and obtains its driver-manager `CancelHandle` before
     calling `AdbcStatementExecuteQuery`. The driver manager invokes
     `AdbcStatementCancel` without taking the ordinary-operation mutex, so a
     different thread can cancel while execution or result retrieval is
     blocked. The handle is statement-scoped and becomes a no-op after that
     statement is released; `connector-adbc` does not call the ADBC C function
     directly or substitute connection-wide cancellation. Between driver
     calls, cancellation is recorded locally and the connector does not issue
     an unnecessary native cancellation callback.
     For a planned read, the SDK's `Connector::read` returns an
     `ExecutionCancel` that forwards to the step in progress: the schema
     check's catalog read, then the query's cancel (with `connector-sql`, the
     `AdbcQueryCancelHandle`). A cancellation recorded between steps keeps the
     next step from starting.
   - **Direct access:** the implementation's `ExecutionCancel` stops file
     reads and source calls in progress.
4. The pull in progress, or the stream's next pull, returns `cancellation`,
   whatever the source returned after being cancelled. A metadata request
   returns `cancellation`.

A cancellation is never reset. Cancelling it stops every catalog read
registered with it, and a later request given it returns `cancellation`
before sending anything; a cancellation that arrives while a catalog read is
being registered stops that read at once.

An operation cancelled before its first pull only records
the cancellation flag; that call returns `cancellation` without sending a
query.

An `ExecutionCancel` is created per execution and stops only that execution. It is
not per session so that a late cancellation of a finished operation cannot stop
the next operation on the same session. `cancel` on a finished execution's
handle does nothing. Other synchronization details between execution and
cancellation are left to the implementation.

For timeouts, the library's watcher thread finds operations past their deadline
and stops them the same way through `ExecutionCancelHandle::expire`, so the
result is `timeout`. A read's deadline counts from the first pull, when the
query is sent, and a metadata request's from the call
([Cancellation and timeouts](connector-contract.md#cancellation-and-timeouts)).
The SDK also checks the deadline whenever a pull starts, but it does not run a
timer per execution; stopping a pull that is blocked at the source is the
watcher's job. Connectors do not set source query time limits, such as
PostgreSQL `statement_timeout`, because cancellation already stops the query at
the source ([Data Source access rules](connector-contract.md#data-source-access-rules)).

For cancellation and timeouts, a direct-access implementation ensures that:

- `cancel` returns without blocking from any thread; and
- after `cancel`, pending source calls are stopped promptly, so the blocked
  pull returns.

### Cleanup

However an operation ends (completion, cancellation, timeout, or error), the
connector SDK:

- releases the stream returned by Data Source access, and with ADBC access also the
  statement;
- with direct access, has the implementation finish open files, source calls,
  and work it started internally, such as background tasks; and
- marks the session unusable if communication with the source broke partway and
  the connection state is unknown, so the next request returns
  `invalid state`.

A stream released partway also stops the source query and releases the stream
returned by Data Source access, so the session accepts the next request
([Requests and results](connector-contract.md#requests-and-results)).

The connector API implementation never lets a Rust panic cross the C ABI. A
panic becomes an `internal` error and marks the session unusable. A pull that
fails with `connection` also marks the session unusable, because the
connection broke partway and its state is unknown.

## ADBC driver handling

This applies only to ADBC access and is implemented by `connector-adbc`.

- **Connector declaration:** a Connector using ADBC access implements
  `AdbcConnector`, which requires its `Connector::Access` to be `Adbc<Self>`
  and declares its driver and supported versions (`DRIVER`) and how connection
  options become driver options (`database_options`). Its error table and
  redaction are its `Connector::ErrorHandler` over `AdbcDiagnostic`. `Adbc<C>`
  implements the SDK's `Access` from this declaration, so loading, opening,
  connecting, and the version check are the same for every such Connector.
- **Loading:** `connector-adbc` loads ADBC drivers through the Rust ADBC driver
  manager from the location received at library initialization, using the
  driver name the connector declares, when the first data source is opened. A
  driver is loaded through its ADBC driver manifest, `<driver name>.toml` in
  that location, which names the shared library and declares the driver's
  version. ADBC reports a driver's version through a connection function
  (`AdbcConnectionGetInfo`), so the version is checked against the supported
  range when the first session opens, returning `invalid settings` if it
  is outside
  ([Calling-convention conformance tests](connector-contract.md#calling-convention-conformance-tests),
  [Data Source access rules](connector-contract.md#data-source-access-rules)).
  A driver that reports its version as `unknown`, as the Apache Arrow ADBC
  PostgreSQL driver does, is identified by the manifest's version instead.
- **Options:** the connector maps product connection options to driver
  options, and the fixed driver options are passed at the same time.
- **Errors:** driver errors carry an ADBC status code, a SQLSTATE, and a
  Data Source-specific vendor code. `connector-adbc` classifies them into contract
  error kinds using the connector's error table and SQLSTATE classes. The
  connector first redacts what its driver copies into diagnostics and must
  not leave the connector, such as SQL text in a driver message. Because
  the SQLSTATE comes from ADBC's error details, SQLSTATE classification lives in
  `connector-adbc`, not `connector-sql`, so ADBC-access connectors that do not
  use `connector-sql` can use it. Errors mid-stream are meant to be retrieved
  in detail with `AdbcErrorFromArrayStream`, but the Rust ADBC driver manager
  does not yet import it
  ([apache/arrow-adbc#4984](https://github.com/apache/arrow-adbc/issues/4984)),
  so every mid-stream error currently arrives as its message alone and is
  classified as `data-source`, even when the source reported a connection
  failure.
- **Our own ADBC drivers:** a source without an existing ADBC driver gets one
  under `adbc-drivers`, built with the Apache Arrow ADBC project's Rust crates
  `adbc_core` and `adbc_ffi`. `adbc_ffi` does not implement
  `AdbcErrorFromArrayStream`, so our drivers implement it themselves, together
  with a wrapper around the Arrow stream that keeps the last error's details.
- **Pinned versions:** `adbc_core`, `adbc_ffi`, and the driver manager crate
  have not reached 1.0, so their versions are pinned and every upgrade is
  checked with the conformance tests
  ([Tests](connector-implementation.md#tests)).

## Credential boundary

| Stage | Rule |
| --- | --- |
| Receive | Values received when opening a data source whose keys the connector marks as secret are placed, on receipt, in a secret-holding type that never prints its value in `Debug` and erases it when dropped |
| Pass | Passed to Data Source access: as ADBC database options with ADBC access; with direct access, to the client the implementation uses |
| Hold | The data source holds them, and sessions use them through their reference to the data source without copying. With ADBC access, the driver also keeps them while the database is alive. |
| Never emit | Not included in errors, logs, metrics, or static information. The connector API has no function that returns a secret value. |
| Discard | Erased when the data source and every session using it are closed, and, with ADBC access, the ADBC database is released |

## Correspondence with the contract

| Contract section | Implemented by |
| --- | --- |
| [Connector API and Data Source access](connector-contract.md#connector-api-and-data-source-access) | `connector-api`, the connector traits, `connector-adbc` ([ADBC driver handling](#adbc-driver-handling)), and direct-access connector implementations |
| [Calling conventions](connector-contract.md#calling-conventions) | The connector SDK's runtime (unit management, session lease, cancellation, timeouts) and its error structure |
| [Metadata retrieval](connector-contract.md#metadata-retrieval) | The Connector's `Catalog` component and source metadata type with its type mapping, and the connector SDK's table metadata model, its comparison, and the source-read boundary |
| [Data read](connector-contract.md#data-read) | The Connector's `Planner` and `Executor` components and value conversion, and the connector SDK's planning and read flows with their schema and lifecycle checks |
| [Read request representation](connector-contract.md#read-request-representation) | The SDK's Substrait validation, `LogicalPlan` conversion, and application of the Connector-supplied rules, plus the Connector's plan-aware semantics |
| [Connector API](connector-contract.md#connector-api) | `connector-api`, its public C header, and its C structures of metadata and comparisons |
