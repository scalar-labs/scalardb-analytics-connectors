# Connector internals

## Summary

This document describes how the connector API library is built to satisfy the
[connector contract](connector-contract.md): crate responsibilities and
dependency rules, how processing is divided between the connector SDK and
connectors, the validated Substrait planning state, the traits and data a
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
| Data Source access | The narrow mechanism that reads a Data Source catalog and runs queries. `connector-adbc` provides ADBC access; a direct-access Connector implements the same boundary itself. |
| Table reference | An unresolved non-empty table name path used by metadata requests. It is not a table binding and carries no proof that it belongs to an open Data Source or registered schema. |
| Table binding | A non-secret opaque identity for one registered table. The Physical Catalog persists it and an adapter copies it into a Substrait `namedTable`; only the Connector interprets it. |
| SQL dialect | Connector-owned representation rules for SQL identifiers and typed literals. It does not represent a Data Source, connection, table, or common SQL grammar. |
| Connector definition | The data part of a connector crate, loaded at build time ([Connector definition](#connector-definition)) |
| Connector trait | The Rust trait a connector implements in code. The connector SDK uses a connector only through this trait and its connector definition ([Connector trait](#connector-trait)). |
| Data Source access trait | The Rust traits defined by `connector-sdk` that only read the Data Source catalog and run queries. `connector-adbc` implements them for ADBC access; a direct-access Connector implements them itself. |
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
| `connector-sdk` | Connector lifecycle, metadata and type-mapping contracts, Substrait validation, complete-request planning contracts, executable-query state, output-schema enforcement, cancellation, deadlines, batch bounds, provenance, error classification, and secret-safe errors | The C ABI, ADBC, native Query Engine plans or residual evaluation, and knowledge of any particular Data Source |
| `connector-adbc` | The ADBC access implementation provided by the connector SDK: loading drivers through the ADBC driver manager and checking their versions, passing options, running queries and receiving Arrow, cancellation, receiving ADBC errors, and classifying errors by SQL-standard SQLSTATE class ([ADBC driver handling](#adbc-driver-handling)) | Deciding types, and knowledge of any particular Data Source |
| `connector-sql` | Processing common to SQL-speaking Data Sources, using `connector-adbc` for Data Source access: running catalog queries and generating SQL from remote `LogicalPlan` values with Data Source-specific dialect and semantic rules | Knowledge of any particular Data Source or native Query Engine plan |
| `crates/connectors/<connector>` | One Connector: its Connector definition and the code the definition cannot express. A direct-access Connector also implements the Data Source access traits. | Knowledge of other Connectors |
| `connector-api` | The connector API's C ABI, registering Connectors and dispatching by Connector, managing the library, Data Sources, sessions, and operations, and assembling static information. It builds the library. | Knowledge of any particular Data Source |
| `adbc-drivers/<connector>` | Our own ADBC drivers, built with `adbc_core` and `adbc_ffi` | Deciding types and interpreting catalogs |
| `conformance` | The runner that tests the contract through the connector API only, and per-Connector fixtures | Dependencies on Connector internals |

The [connector contract](connector-contract.md) is the semantic source of truth.
Rust crates enforce the implemented behavior, and callers in other languages,
such as adapters and the Physical Catalog, use the typed connector API.

### Dependency rules

~~~text
connector-api --> connectors/<connector> (SQL family) --> connector-sql --> connector-adbc --> connector-sdk --> analytics-types
connector-api --> connectors/<connector> (direct access) ----------------------------------> connector-sdk
connector-api --> connector-sdk
connector-adbc --> ADBC driver manager --(loaded at run time)--> ADBC driver
conformance --> connector-api C ABI and contract
~~~

- The three connector SDK crates do not depend on connector crates.
- `connector-sdk` does not depend on ADBC. The ADBC driver manager appears only
  below `connector-adbc`.
- Connectors do not depend on one another. Processing two connectors need
  belongs in a connector SDK crate.
- Connector crates do not depend on native libraries. Native dependencies stay
  inside ADBC drivers
  ([Connector API and Data Source access](connector-contract.md#connector-api-and-data-source-access),
  [Data Source access rules](connector-contract.md#data-source-access-rules)).
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
  example, the PostgreSQL connector runs its tests and the PostgreSQL
  conformance tests without building other connectors.
- **Addition:** adding a Connector is completed by adding a connector crate,
  the Connector's type mapping, and, with ADBC access, its ADBC driver. The
  connector SDK, the C ABI, and its callers do not change; `connector-api`
  gains one feature without code changes, and the library MINOR is raised
  ([Compatibility and versions](connector-contract.md#compatibility-and-versions)).

Both hold with a single artifact. The main effect of one library is that
delivering a new connector requires a library release, and because the library
ships inside the application distribution, that release is a product release.

Version and assembly rules:

- `connector-sdk` uses semantic versioning. Each connector declares the range
  of `connector-sdk` versions it supports.
- Each `connector-api` version fixes one `connector-sdk` major. Connector
  traits of different majors are different types, so one `connector-api` cannot
  call connectors built against different majors.
- An incompatible `connector-sdk` change first adds the new API within the
  current major and deprecates the old one, keeping both. After every connector
  has moved to the new API, the old one is removed and the major is raised.
  Each connector can follow at its own pace, and a connector that lags does not
  drop out of the artifact.
- Each connector crate has its own version and raises it whenever its mapping
  or coverage changes
  ([Type mappings and coverage](connector-contract.md#type-mappings-and-coverage)).
  That version is published as the connector's name and version in the
  capability declaration, so retrieval info and read provenance record which
  connector version produced a result even with a single artifact.
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
| Capability declaration | Assembling metadata declarations and conservative pushdown hints | Metadata coverage and certainly-unsupported hints for the connected source's version and settings |
| Metadata retrieval | Running catalog queries (`connector-sql` for SQL sources), applying the type mapping, deciding unsupported columns, attaching retrieval info, aggregating completeness, the digest, and converting output to Arrow | Catalog queries, parsing Data Source types, how constraints and identifiers are read, and rules for detecting missing information |
| Data read | Offered Substrait validation and `LogicalPlan` conversion, initial `QueryPlan` construction, application of the Connector-supplied rule set, decomposition-contract assertions, cancellation, deadlines, batch bounds, and provenance | Obtaining current Data Source/session facts, supplying remote/local rewrite rules, generating and executing remote queries, guaranteeing rewrite semantics, and converting source output |
| Calling conventions | Cancellation and timeouts, the error structure, and credential handling; with ADBC access, loading and version-checking the ADBC driver (`connector-adbc`) | Mapping connection options to driver options, fixed settings, and classifying Data Source-specific error codes; with direct access, the Data Source access trait implementation |

The rule is that whatever must apply with the same meaning to every connector
belongs in the connector SDK, and whatever needs source knowledge belongs in the
connector. Data Source access sits behind a narrow trait that only reads catalogs
and runs queries ([Connector trait](#connector-trait)), so common validation,
type mapping, planned-state checks, read provenance, and table metadata assembly
have the same SDK boundary in both forms. With ADBC access, connection, query execution, Arrow results,
cancellation, and error retrieval are also the same `connector-adbc` processing
for every source, leaving only source knowledge to the connector. With direct
access, the connector also implements the Data Source access traits.

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
ExecutableQuery<ConnectorQuery> {
    query: ConnectorQuery
}

PlannedPushdown {
    remote_plan: Substrait Plan
    local_plan: optional Substrait Plan
    executable_query
    base_schema: Analytics schema
    remote_output_schema: Analytics schema
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
generation, and then dropped. It does not appear in `ConnectorPlanner`,
`PlannedPushdown`, or the language-neutral connector API.

The Connector owns any source checks or execution guards required to preserve
the semantics of a rewrite. The SDK does not encode those checks as a common
fingerprint or restore protocol.

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

A connector enters the SDK through a process-level library, a long-lived source
handle, and an isolated session. The session supplies catalog, planning, and
execution capabilities:

~~~text
ConnectorLibrary:
    open_data_source(data_source_config) -> DataSource

DataSource:
    open_session(session_config) -> DataSourceSession

DataSourceSession:
    catalog() -> CatalogReader
    planner() -> ConnectorPlanner
    executor() -> ConnectorExecutor

ConnectorPlanner:
    plan(offered_plan)
        -> Planned(PlannedPushdown) | Unsupported

RewriteRuleConnectorPlanner:
    load_metadata(offered_plan) -> Connector-private metadata
    rewrite_rules() -> RewriteRuleSet<Connector-private metadata>
    create_executable_query(remote_plan, metadata) -> ExecutableQuery<ConnectorQuery> | Unsupported

SQL-family create_executable_query:
    connector-sql translates remote_plan using physical names from metadata
        common visitor owns Read/Project/Filter and fixed SQL syntax
        LogicalExpression exposes resolved functions, fields, and validated LiteralValue nodes
        SqlDialect renders identifier and LiteralValue fragments
        Fragment composes them into one RenderedQuery
    ExecutableQuery<RenderedQuery> retains that typed result for execution

SQL-family execution:
    connector-sql passes the rendered statement to connector-adbc::query
    connector-adbc owns lazy statement creation, execution, streaming, cancellation,
        diagnostics, and resource release

Connector SDK implementation of ConnectorPlanner for RewriteRuleConnectorPlanner:
    metadata = load_metadata(offered_plan)
    query_plan = initial complete remote/local QueryPlan
    for rule in rewrite_rules() in configured order:
        query_plan = rule.rewrite(query_plan, metadata)
        unchanged query_plan -> no-op and continue
        error -> stop with error
    executable_query = create_executable_query(query_plan.remote_plan, metadata)
    unsupported -> return Unsupported without constructing a pushdown
    convert the remote/local LogicalPlans to Substrait
    return PlannedPushdown

ConnectorExecutor:
    execute(planned_pushdown, execution_context) -> ExecutionOutput
~~~

Connector planning runs after the SDK has produced a `LogicalPlan`, because
that is the first point at which the Connector can know which source facts are
needed. It may query the Data Source under the planning operation's ordinary
timeout, cancellation, error, and session-lease rules. Implementations may
reuse cached facts across requests, but no planning-snapshot identity is
required or serialized to workers.

The basic `ConnectorPlanner` interface gives a Connector full control of those
steps. For the standard rule-based case, `RewriteRuleConnectorPlanner` lets the
Connector provide its private metadata loader, ordered rules, and executable
query generator while the SDK owns the common control flow. Its blanket
`ConnectorPlanner` implementation returns `PlannedPushdown` directly; no
resolved-plan carrier or second planning result crosses the boundary. The SDK
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

If a rewrite depends on mutable Data Source semantics, the Connector must use a
transaction, snapshot, immutable session setting, version check, or another
source-specific mechanism sufficient for that rule. This remains Connector
behavior rather than an SDK-wide planning data model.

The pseudocode describes responsibilities, not required Rust method names or
whether translation and execution are separate implementation calls. The
public connector API remains the language-neutral boundary; these internal
interfaces keep source knowledge out of the SDK.

For SQL-family Connectors, `connector-sql` traverses the validated remote
`LogicalPlan` and its `LogicalExpression` nodes. It does
not interpret Substrait anchors or extension declarations. `load_metadata` creates one
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
`SqlDialect` owns actual representation differences such as identifier quoting
and literal spelling and escaping. The result is one complete
non-parameterized SQL statement. Connector rewrite rules use the concrete Data Source type,
nullability, collation, version, and session facts in the shared metadata before
leaving an expression in the remote plan. SQL translation renders that approved
plan without repeating the semantic decision. The resulting
`ExecutableQuery<RenderedQuery>` retains that SQL-specific type through
adoption. Execution sends its SQL text directly; it does not receive or
reconstruct a `LogicalPlan`, planning metadata, dialect, or semantic policy.

Data Source access interfaces sit below these public contracts. They read catalogs
and execute Data Source queries returning Arrow streams; they do not expose native
Query Engine plans. `connector-adbc` provides the common ADBC implementation,
while a direct-access connector provides its own. Cancellation remains scoped
to one execution so another thread can stop a blocked source call.

`connector-sdk` defines its own traits instead of using `adbc_core`'s traits
because:

- `adbc_core`'s traits have associated types and generic arguments and cannot
  be used as trait objects, while `connector-sdk` must hold each connector's
  different implementation behind one type;
- `adbc_core`'s traits have no `Send` and `Sync` bounds;
- `adbc_core`'s cancellation takes `&mut self`, so a running statement cannot
  be cancelled from another thread; and
- `adbc_core` has not reached 1.0 and releases incompatible versions every few
  months, while its own traits confine those changes to `connector-adbc`.

The `CatalogReader` returned by a connector's `catalog` has one function per
metadata request
([Capabilities](connector-contract.md#capabilities)):

~~~rust
pub trait CatalogReader: Send + Sync {
    /// Namespace list. `filter` is a set of names matched exactly, without case folding;
    /// filter at the source if possible, otherwise after listing.
    fn list_namespaces(&self, session: SessionLease, filter: &NameFilter)
        -> Result<CatalogRead<Listed<NamespacePath>>>;

    /// Table list across namespaces, with a namespace filter and a table-name filter.
    fn list_tables(&self, session: SessionLease,
                   namespaces: &NameFilter, tables: &NameFilter)
        -> Result<CatalogRead<Listed<TableEntry>>>;

    /// Table metadata for a set of tables: one record per requested table.
    fn read_tables(&self, session: SessionLease, tables: &[TableRef])
        -> Result<CatalogRead<TableRecord>>;
}

/// The session lent to one operation. Gives `&mut dyn DataSourceSession` access, and goes
/// back to the session when the operation releases its Data Source resources.
pub struct SessionLease { /* ... */ }

/// One catalog read, owned by the operation. Borrows nothing from the reader,
/// the session, or the arguments; sends nothing to the source before the first pull.
pub struct CatalogRead<T> {
    pub items: Box<dyn Iterator<Item = Result<T>> + Send>,
    /// Stops this catalog read, including the source execution in progress.
    pub cancel: Arc<dyn CancelHandle>,
}

/// None means no filter.
pub type NameFilter = Option<BTreeSet<String>>;

pub enum Listed<T> {
    Item(T),
    /// A range whose list could not be read.
    Unreadable { scope: NamespacePath, reason: MissingReason },
}

pub struct TableEntry { pub table: TableRef, pub kind: TableKind }

/// The three states of one requested table.
pub enum TableRecord {
    Read(DataSourceTable),
    NotFound { table: TableRef },
    Unavailable { table: TableRef, reason: MissingReason },
}

/// What the Data Source declares, as reported by the Data Source.
pub struct DataSourceTable {
    pub table: TableRef,
    pub kind: TableKind,
    pub columns: Vec<DataSourceColumn>,
    /// True if the catalog may have hidden columns without privilege.
    pub columns_may_be_hidden: bool,
    pub constraints: Reported<Vec<DataSourceConstraint>>,
    pub data_source_id: Reported<DataSourceId>,
    pub schema_version: Reported<DataSourceSchemaVersion>,
    pub read_at: Timestamp,
}

pub struct DataSourceColumn {
    pub name: String,
    pub ordinal: u32,
    pub nullable: bool,
    pub data_source_type: Result<String, MissingReason>,  // missing makes the table incomplete
    pub attrs: DataSourceColumnAttributes,
    pub data_source_id: Reported<DataSourceId>,
}

/// An item the connector may fail to read or may declare it does not offer.
pub enum Reported<T> {
    Value(T),
    Missing(MissingReason),
    NotOffered,
}
~~~

Each `CatalogReader` function takes a session lease from the operation and
returns a `CatalogRead`. A `CatalogRead` borrows nothing from the reader, the
session, or the arguments, so the operation can own it as a Data Source
resource ([Lifetimes](#lifetimes)). Even when one `CatalogRead` runs several
executions at the source, such as one query per namespace, its `cancel` stops
the execution in progress.

The three `TableRecord` states correspond one to one to the contract's table
metadata row states: `Read` is complete without missing items and incomplete
with them, `NotFound` is "does not exist", and `Unavailable` is "could not be
read" ([Missing information](connector-contract.md#missing-information)). The
three `Reported` states correspond to a value, a missing item, and "not
offered". The connector only puts what the source reported into these records.
Applying the type mapping, deciding unsupported columns, aggregating
completeness, attaching retrieval info, and computing the digest are done by
the connector SDK from these records, never by the connector
([Division of processing](#division-of-processing)).

## Connector definition

The connector definition is a file in the connector crate, such as TOML, loaded
at build time. `connector-sdk` defines its format and validates it when loading.

| Part | Content |
| --- | --- |
| Data Source access | The access form and which options are secret. With ADBC access, the ADBC driver's name and supported version range, and how product connection options map to driver options. |
| Session | Setting queries run right after connecting, and the driver options or reader crate settings to fix |
| Catalog | Queries that read namespaces, tables, columns, constraints, and identifiers, and the meaning of their result columns (SQL sources) |
| Types | Per mapping rule, the Arrow type the Data Source access returns and the name of the building block that converts it to the Analytics type |
| Dialect | Identifier quoting and typed-literal spelling and escaping (SQL sources) |
| Semantics | Guards that prove an operator preserves meaning for concrete Data Source types, nullability, collations, versions, and session facts; an unproved case remains local or is `Unsupported` |
| Cancellation | With ADBC access, the means sent to the source when the driver's cancellation does not stop the query at the source |
| Errors | The classification of Data Source-specific error codes into contract error kinds |

Whatever can be data is data. Dialect translation tables, semantic guards, and type correspondence are
easier to check for errors as tables than scattered through code, and type data
can be checked against [Type mappings](type-mappings.md) and conformance
expectations.
What data cannot express, such as Data Source type grammar, type resolution needing
extra catalog reads, and processing for non-SQL sources, is code. Writing
everything as data would push the definition format toward a programming
language.

## Connector API implementation

`connector-api` implements the [connector API functions](connector-contract.md#functions):

- **Registration:** connectors in the artifact are registered at build time
  and selected by features.
- **Dispatch:** opening a data source selects the connector for the named
  Connector. That connector handles all sessions and operations on that data
  source.
- **Static information:** assembled from the registered connectors'
  `ConnectorDescriptor` values.
- **Unit management:** the library, data sources, sessions, operations, and
  result streams are managed as described in [Lifetimes](#lifetimes).

## Lifetimes

| Unit | Created | Holds | Released |
| --- | --- | --- | --- |
| Library | At initialization | Settings, the ADBC driver manager (`connector-adbc`), and the timeout watcher thread | At shutdown. Loaded ADBC driver shared libraries stay loaded until the process exits ([Resource ownership](#resource-ownership)). |
| Data source | When opened | The selected connector, validated connection settings and credentials, and its `DataSourceAccess` (an ADBC database with ADBC access) | When the data source and every session using it are closed |
| Session | When opened | Its `DataSourceSession` (an ADBC connection with ADBC access), catalog reader, connector planner, executor, and optional planning-facts cache | When closed, releasing its cache and `DataSourceSession` |
| Operation | When started: validates the request and takes a session lease | Data Source resources: the session lease and, after resolution or first pull, its execution/read state and `CancelHandle`; also cancellation, deadline, error, and final read-provenance state | When closed, releasing every Data Source resource and operation-owned provenance. Metadata/read resources are also released, and the lease returned, when the stream ends or is released. |
| Result stream | When a metadata or read operation starts | A reference to the operation, conversion state, expected output schema, cancellation/deadline state, and batch bounds. It holds no Data Source resources. | On the stream's `release`; the operation stops the Data Source query and releases the stream returned by Data Source access |

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
| Strings and bytes the caller passes (option values, Substrait plans) | The caller | The library copies them during the call and does not refer to them afterwards |
| Error structures | The library | Valid until the caller calls the release function; an operation's error is valid until the operation is closed |
| Read provenance JSON | The read operation | Becomes final when execution ends and remains retrievable until the operation is closed; it is not Arrow schema metadata |
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

The Data Source access traits are synchronous. ADBC is a synchronous C API, and the
connector SDK has no asynchronous runtime. Starting an operation only validates
the request (including the checks in [Read representation](#read-representation))
and does not call Data Source access. Read and metadata Data Source access is
called from the caller's thread inside a stream pull; planning resolution may
also call it from `plan_pushdown`. At the first pull, the connector SDK translates the
request, calls `read_catalog` or `execute`, stores the returned execution and its
`CancelHandle` in the operation, and then pulls its stream. A direct-access
implementation that uses an asynchronous reader crate keeps that runtime inside
the implementation and waits for results within the trait call; the runtime
never leaves the trait.

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

A data source uses its `DataSourceAccess` from several threads. `DataSourceAccess` is
`Send + Sync`, and a direct-access implementation must behave correctly when
sessions are opened concurrently. ADBC does not require drivers to support
concurrent use of a database, so `connector-adbc` serializes database
functions (creating connections). The `Connector` trait is also `Send + Sync`.

### Cancellation and timeouts

Cancellation propagates in this order:

1. The caller cancels an operation from any thread.
2. The connector SDK calls `cancel` on that operation's execution
   `CancelHandle`.
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
   - **Direct access:** the implementation uses that execution's
     `CancelHandle` to stop file reads and source calls in progress.
4. The stream's next pull returns `cancellation`.

An operation cancelled before its first pull only records
the cancellation flag; that call returns `cancellation` without sending a
query.

A `CancelHandle` is created per execution and stops only that execution. It is
not per session so that a late cancellation of a finished operation cannot stop
the next operation on the same session. `cancel` on a finished execution's
handle does nothing. Other synchronization details between execution and
cancellation are left to the implementation.

For timeouts, the library's watcher thread finds operations past their deadline
and stops them the same way, returning `timeout`. The deadline counts from the
first pull, when the query is sent
([Cancellation and timeouts](connector-contract.md#cancellation-and-timeouts)).
When a source has a query time limit, such as PostgreSQL `statement_timeout` or
MySQL `max_execution_time`, the connector also sets it to match the deadline.
The connector's own deadline stops operations even when the source limit does
not take effect. Whichever fires first, the result is `timeout` if the deadline
has passed.

For cancellation and timeouts, a direct-access implementation ensures that:

- `cancel` returns without blocking from any thread;
- after `cancel`, pulls of that execution's stream promptly end with
  `cancellation`, and pending source calls are stopped; and
- when the source API has a call time limit, it is set to match the deadline.

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
panic becomes an `internal` error and marks the session unusable.

## ADBC driver handling

This applies only to ADBC access and is implemented by `connector-adbc`.

- **Loading:** `connector-adbc` loads ADBC drivers through the Rust ADBC driver
  manager from the location received at library initialization, using the
  driver name in the connector definition when a data source is opened. ADBC
  reports a driver's version through a connection function
  (`AdbcConnectionGetInfo`), so the version is checked against the supported
  range when the first session opens, returning `invalid settings` if it
  is outside
  ([Calling-convention conformance tests](connector-contract.md#calling-convention-conformance-tests),
  [Data Source access rules](connector-contract.md#data-source-access-rules)).
- **Options:** product connection options are mapped to driver options as the
  connector definition says, and the fixed driver options are passed at the
  same time.
- **Errors:** driver errors carry an ADBC status code, a SQLSTATE, and a
  Data Source-specific vendor code. `connector-adbc` classifies them into contract
  error kinds using the definition's error table and SQLSTATE classes. Because
  the SQLSTATE comes from ADBC's error details, SQLSTATE classification lives in
  `connector-adbc`, not `connector-sql`, so ADBC-access connectors that do not
  use `connector-sql` can use it. Errors mid-stream are retrieved in detail with
  `AdbcErrorFromArrayStream`; when a driver does not support it, the error is
  classified as `data-source` from its message alone.
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
| Pass | Passed to Data Source access: as ADBC database options with ADBC access, also used when opening a separate connection for cancellation; with direct access, to the client the implementation uses |
| Hold | The data source holds them, and sessions use them through their reference to the data source without copying. With ADBC access, the driver also keeps them while the database is alive. |
| Never emit | Not included in errors, logs, metrics, read provenance, capability declarations, or static information. The connector API has no function that returns a secret value. |
| Discard | Erased when the data source and every session using it are closed, and the `DataSourceAccess` (the ADBC database with ADBC access) is released |

## Correspondence with the contract

| Contract section | Implemented by |
| --- | --- |
| [Connector API and Data Source access](connector-contract.md#connector-api-and-data-source-access) | `connector-api`, the Data Source access traits, `connector-adbc` ([ADBC driver handling](#adbc-driver-handling)), and direct-access connector implementations |
| [Calling conventions](connector-contract.md#calling-conventions) | `connector-api` unit management and the connector SDK's cancellation, timeout, and error structure |
| [Capability declaration](connector-contract.md#capability-declaration) | Metadata coverage and conservative pushdown hints assembled from connector information |
| [Metadata retrieval](connector-contract.md#metadata-retrieval) | The connector's `catalog` (`CatalogReader`) and `parse_source_type`, and the connector SDK's type mapping and table metadata assembly |
| [Data read](connector-contract.md#data-read) | The connector's planner and executor, and the connector SDK's schema, lifecycle, batch-bound, and provenance checks |
| [Read request representation](connector-contract.md#read-request-representation) | The SDK's Substrait validation, `LogicalPlan` conversion, and application of the Connector-supplied rules, plus the Connector's plan-aware semantics |
| [Connector API](connector-contract.md#connector-api) | `connector-api` and its public C header |
