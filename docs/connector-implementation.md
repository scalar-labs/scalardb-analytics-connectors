# Connector implementation conventions

## Summary

This document defines the conventions for implementing one Connector's
connector on the structure in [Connector internals](connector-internals.md):
how a connector is built, how metadata is retrieved, types and values, reads
and plan-aware pushdown decisions, errors and logs, tests,
choosing the form of Data Source access and an ADBC driver, direct access, isolating
native dependencies, the initial PostgreSQL and MySQL conventions, and how the
conventions apply to other sources. What is promised to callers is defined by
the [connector contract](connector-contract.md); this document defines how to
meet it. Terms follow the [vocabulary](connector-internals.md#vocabulary) of
Connector internals.

## Building a connector

A connector for a new Connector is built in this order:

1. Confirm that the Connector has a type mapping in
   [Type mappings](type-mappings.md); if not, define it first. There is one type
   mapping per Connector and a connector does not invent its own. If the
   mapping uses a new extension-encoded type, register it in the
   [Analytics type model](analytics-types.md#registered-extension-encoded-types)
   first.
2. Choose the form of Data Source access ([Choosing Data Source access and ADBC drivers](#choosing-data-source-access-and-adbc-drivers)).
   For ADBC access, choose the ADBC driver, and if no existing driver is usable,
   write one under `adbc-drivers/<source>` ([Our own ADBC drivers](#our-own-adbc-drivers)).
   For direct access, choose the reader crates.
3. Create `crates/connectors/<source>` and write its
   [connector definition](connector-internals.md#connector-definition).
4. Implement what the definition cannot express through the
   [connector trait](connector-internals.md#connector-trait). For a source that
   speaks SQL, use `connector-sql`'s default `catalog` and `translator`. For
   direct access, also implement the Data Source access traits
   ([Direct access](#direct-access)).
5. Fill `ConnectorDescriptor` with the name, version, Connectors, supported
   `connector-sdk` version range, the description of accepted options
   ([Static information](connector-contract.md#static-information)), the form
   of Data Source access, and, with ADBC access, the ADBC driver.
6. Put fixtures and expectations in `conformance/fixtures/<source>` and pass, for
   this connector alone, the calling-convention conformance tests and the
   metadata and data read conformance tests ([Tests](#tests)).
7. Register it as a `connector-api` feature and build it into the artifact.

These steps change neither the connector SDK, the contract, nor the callers of
the connector API; `connector-api` only gains a feature, without code changes
([Build cycle and versions](connector-internals.md#build-cycle-and-versions)).

## Retrieving metadata

### Stages

Table metadata is produced in these stages:

| Stage | Performed by | Input | Output |
| --- | --- | --- | --- |
| 1. Read the catalog | The connector SDK, running the definition's queries (SQL sources) or the connector's code | A set of tables | Raw records of the source declaration: names, ordinals, nullability, Data Source types, constraints, identifiers, and missing items |
| 2. Parse Data Source types | The connector's code | Data Source types, and catalog information needed to tell type kinds apart | Keys for looking up the type mapping |
| 3. Apply the type mapping | The connector SDK | Keys and the connector's coverage | Analytics types, or unsupported with a reason |
| 4. Assemble table metadata | The connector SDK | Results of stages 1 to 3 and retrieval info | Table metadata and its content digest |
| 5. Convert to Arrow | The connector SDK | Table metadata | Rows of the table metadata result |

A connector supplies only the stage 1 queries and stage 2, and never decides
Analytics types itself, so that the type mapping is applied by the same rules
for every Connector.

In stage 2, the Data Source type string alone sometimes cannot identify the mapping
rule. PostgreSQL `format_type` returns only the name of a domain or enum, such
as `mood`. In that case the connector reads the type kind and base type from
the catalog (`typtype` and `typbasetype` in `pg_type`) so that a domain finds its
base type's rule and an enum finds the enum rule. The Data Source type keeps the
domain or enum name, as the type mapping says.

### Reading the catalog

- Data Source types are read from the origin the type mapping names
  ([Common rules](type-mappings.md#common-rules)), never substituted by ADBC
  `GetObjects` or a client library's metadata API, which report types by the
  driver's rules. With direct access, when a reader crate returns the type found
  in table-format metadata or a file footer unchanged, that type is the origin's
  value.
- A connector must know whether the catalog it reads silently omits objects
  without privilege. If it reads such a catalog and cannot confirm that nothing
  was omitted, it returns the table metadata as incomplete
  ([Missing information](connector-contract.md#missing-information)). The same
  knowledge decides missing tables: not found in a catalog that does not omit
  objects is "does not exist"; not found in one that does is "could not be read".
- Name filters are applied in the source query when possible, and after listing
  otherwise ([Metadata requests](connector-contract.md#metadata-requests)).
- Excluded namespaces are listed as the connector's own rule
  ([Data Source types](connector-contract.md#data-source-types)).
- When a source cannot list tables of several namespaces in one query, the
  connector SDK repeats the query per namespace; the definition only needs the
  per-namespace query.
- The retrieval time is the time each table's catalog was read.

### Retrieval info and digest

The connector SDK takes the following retrieval info items from the connector's
`ConnectorDescriptor`, its definition, the ADBC driver's report (ADBC access),
and the session's capability declaration:

- The connector's name and version and, with ADBC access, the ADBC driver's name
  and version. Direct-access reader crates are identified by the connector
  version
  ([Retrieval info and content digest](connector-contract.md#retrieval-info-and-content-digest)).
- The library version
  ([Compatibility and versions](connector-contract.md#compatibility-and-versions)).
- The fixed settings identifier: a digest of the normalized list of session
  setting queries, fixed driver options, and fixed reader crate settings.

The content digest is computed by the connector SDK with the contract's
normalization rules.

## Types and values

### Coverage

A connector may cover only mapping rules that satisfy both:

- In data read, the values Data Source access returns can be turned into the rule's
  Analytics values using only conversions that do not change values.
- The data read conformance tests pass for the rule, including its boundary
  values.

When coverage or a rule's mapping changes, the connector raises its version and
records the change in its changelog
([Type mappings and coverage](connector-contract.md#type-mappings-and-coverage)).

### Converting Data Source access output to Analytics types

- The connector definition lists, per mapping rule, the Arrow type the source
  access (ADBC driver or reader crate) returns and the building block that
  converts it to the Analytics type. Building blocks are chosen from the
  value-preserving conversions the connector SDK provides, such as widening
  integers, converting decimal strings to `decimal128`, and aligning time units.
- When Data Source access already returns the Analytics type, nothing is converted
  and the returned buffers pass through to the caller.
- Values that cannot be carried exactly are a `data` error
  ([Values](connector-contract.md#values)).
- Types for which the ADBC driver or reader crate performs a value-changing
  conversion (rounding, truncation, substitution of a nearby value) are not
  covered. When a driver option or reader crate setting can disable that
  conversion, the disabling setting is fixed.

### Fixed settings

Two kinds of settings that change how types and values are produced are fixed,
and the fixed settings appear in the capability declaration
([Cancellation, errors, and declarations for metadata](connector-contract.md#cancellation-errors-and-declarations-for-metadata)):

- session settings run right after connecting, such as the time zone; and
- ADBC driver options or reader crate settings that change how types and values
  are produced.

Connection settings passed by the caller cannot override fixed settings.

## Reading data

### Translation

- Filter literals in the remote Substrait plan are passed as parameters, never
  embedded in the query text. Embedding them would let quoting errors change the
  query's meaning and leave literals in logs.
- A parameter's type is one that the source compares with the column's type
  without a type conversion error.
- One read request becomes one query to the source. Splitting it into several
  queries would break the declaration that one request reads a single
  point-in-time state ([Consistency](connector-contract.md#consistency)).
  Auxiliary queries that read attributes or types from the catalog for planning
  or for the schema check
  ([Plan-aware support decisions](#plan-aware-support-decisions),
  [Type information for the schema check](#type-information-for-the-schema-check))
  read no rows and are outside this rule.
- Planning constructs the `ExecutableQuery` that implements the remote plan.
  Execution sends that generated query directly rather than retranslating the
  Substrait plan. When work remains local, the Connector returns a `local_plan`
  as part of its proposal. The Adapter adopts the proposal only if it can
  convert the complete local plan to native Query Engine work. The connector
  SDK does not apply a leftover expression after the source returns Arrow.
- Limit, aggregation, sort, TopN, and join translation are not part of the
  initial implementation. Their planner rules stay `Unsupported` until their
  end-to-end semantics and execution paths exist.

### Plan-aware support decisions

The standard implementation uses `RewriteRuleConnectorPlanner`. The Connector
defines a private metadata type, loads the source facts needed by the
offered `LogicalPlan`, supplies an ordered `RewriteRuleSet<Metadata>` over
`LogicalPlan`, and generates the executable query from the final remote logical
plan. The SDK implements the common `ConnectorPlanner` flow around those three
operations, including constructing `LogicalPlan` while validating Substrait
and converting the result back to Substrait. A Connector that needs a
different flow may implement the basic `ConnectorPlanner` interface directly.
The decision is not made from an operator/type capability table alone. The
shared contract does not require a filter to be split into conjuncts or
normalized to CNF or DNF.

`RewriteRuleSet` evaluates rules in configured order. Every rule converts one
complete `QueryPlan` into another; returning the input unchanged is a no-op, and
an error stops planning. The initial `QueryPlan` contains a remote full-column
`Read` and a local `RemoteResult -> Filter -> Project` logical plan, so the
complete execution path works before source-specific pushdown is implemented.
Each rule receives a `RewriteContext` that borrows the offered `LogicalPlan`
and the Connector's private metadata. The metadata never becomes part of
`QueryPlan` or `PlannedPushdown`.

Translation tables and conservative semantic guards are still useful connector
implementation tools. They enumerate only combinations known to be safe, but
their result is input to the plan-aware decision rather than an authorization
visible to the adapter. An overlooked collation, Data Source type, or session
setting must prevent the affected rewrite rule from being applied. The
affected rule remains a no-op, and a later Connector-supplied rule may produce
an appropriate local plan. A custom `ConnectorPlanner` may instead return
`Unsupported`; it must never produce a silently different decomposition.

The source attributes used for the decision, such as Data Source type, collation,
character set, and Data Source version, must be valid for the planned execution.
After common validation, Connector planning interprets opaque table bindings and
reads the catalog or source facts required by that complete region. This operation
may perform timeout-bound, cancellable Data Source I/O. A Connector may prefetch
or cache facts for a session, but the cache is an implementation strategy rather
than a required planning-snapshot abstraction.

On the first pull, before sending the data query, the connector refreshes those
facts with a metadata preflight unless the session or a named snapshot
guarantees that they cannot have changed. Preflight also establishes an
execution guard that remains effective while the source evaluates accepted
operators: for example a transaction/snapshot, immutable session settings, a
version token validated in the same operation, or semantics encoded explicitly
in the generated request. If no such guard is possible, the connector does not
apply a rewrite whose correctness depends on the mutable fact. A rewrite with a
local plan is valid only if every permitted change still makes the remote result
safe input for that local plan. Result type information remains part of the
schema check, but information available only
after the data query starts cannot justify an earlier rewrite decision.

If confirmation shows a change, the connector must not execute the previously
planned remote query under the old assumptions. It refreshes the source facts
and requires replanning; if the change also changes an Analytics type, the
result is `stale registration`.

Semantic guard tables are managed with the connector's version. Data-read
conformance tests use fixtures whose comparison meaning varies to prove fully
remote rewrites, rewrites with local work, and unsupported outcomes.

### Type information for the schema check

The connector SDK obtains the Data Source types of a query's result in the same form
as catalog Data Source types and uses them for the
[schema check](connector-contract.md#schema-check):

- With ADBC access, when the ADBC driver puts the Data Source type name in the
  result's Arrow field metadata, that is used. The connector definition names
  the metadata key.
- With direct access, the type is obtained, in the same form as the catalog
  Data Source type, from the schema the reader crate read at read time, such as
  table-format metadata or a file footer.
- When the driver does not provide type names, or the result's type information
  cannot identify the mapping rule (PostgreSQL domains and enums, MySQL `ENUM`),
  the column's type is read from the catalog before the query.

### Batches

The Data Source access batch size is set to the library's configured maximum
([Functions](connector-contract.md#functions)). When the driver or reader crate
cannot set it, the connector SDK splits batches itself.

## Errors, logs, and metrics

### Error classification

The connector SDK classifies Data Source access errors into the contract's
[error kinds](connector-contract.md#errors). Direct-access errors are
classified by the connector's `classify_error`. The rest of this section covers
ADBC access. ADBC errors carry a status code, a SQLSTATE, and a Data Source-specific
vendor code. Classification by vendor code (the definition's error table) is
applied before classification by SQLSTATE class; for example, MySQL `1142`
returns SQLSTATE `42000` but is classified as `permission`.

| SQLSTATE (class or code) | Kind | Classified in |
| --- | --- | --- |
| `08` (connection exception) | `connection` | `connector-adbc` |
| `28` (invalid authorization specification) | `authentication` | `connector-adbc` |
| `22` (data exception) | `data-source`; `internal` when caused by a parameter the connector passed (a wrong parameter type) | `connector-adbc`, and the connector SDK, which judges the cause |
| `40` (transaction rollback) | `data-source`, retryable | `connector-adbc` |
| `42` (syntax error or access rule violation) | `internal` (a defect in the query the connector built), except where a vendor code identifies `permission` or `stale registration` | `connector-adbc` |
| Others | `data-source`, not retryable | `connector-adbc` |
| PostgreSQL `42501` (insufficient privilege) | `permission` | PostgreSQL connector error table |
| PostgreSQL `42P01` (undefined table), `42703` (undefined column) | `stale registration` | PostgreSQL connector error table |
| MySQL `1146` (no such table, SQLSTATE `42S02`), `1054` (unknown column, SQLSTATE `42S22`) | `stale registration` | MySQL connector error table |
| PostgreSQL class `53` (insufficient resources) | `resource` | PostgreSQL connector error table |
| PostgreSQL `57014` (query canceled) | `cancellation` if a cancellation was received, `timeout` if the deadline has passed, otherwise `data-source` | PostgreSQL connector error table |

An error with ADBC status `ADBC_STATUS_IO` and no SQLSTATE (connection broken or
impossible) is `connection`. Errors detected by the connector SDK itself are
classified separately from source errors. A value that cannot be carried exactly
is `data` ([Values](connector-contract.md#values)), and `data` is used only for
that case. If execution cannot consume a `PlannedPushdown` that the same
Connector produced, the mismatch is `internal`; rejecting an offered region
during planning is instead the normal `Unsupported` outcome.

### Logs, metrics, and diagnostics

The connector SDK and connectors emit logs and metrics as Rust `tracing`
events. Destination and level are chosen by library settings
([Functions](connector-contract.md#functions)); how the application collects
them belongs to the application.

What may and may not be emitted is defined in
[Errors](connector-contract.md#errors). It is implemented as follows:

- Secret values are held in the secret-holding type
  ([Credential boundary](connector-internals.md#credential-boundary)) and never
  appear in log strings, even through `Debug`.
- Filter literals and parameter values are never fields of log events.
- Error messages returned by the data source appear only in the error
  structure's source message, never in logs.
- What may be emitted: connector name and version, ADBC driver name and version,
  Connector, Data Source product name and version, host name, port, and database
  name of the connection, request kind, table and column names, duration, row
  count, byte count, batch count, error kind, retry flag, and source error code.

Logs emitted by ADBC drivers and reader crates are outside the connector SDK's
control, so keeping secrets and literals out of their logs is a selection
criterion for them
([Choosing Data Source access and ADBC drivers](#choosing-data-source-access-and-adbc-drivers)).

That secrets do not leak is checked by the calling-convention conformance tests,
which require that secret values never appear anywhere in logs or errors.

## Tests

### Test kinds

| Test | Checks | How it runs |
| --- | --- | --- |
| Calling-convention conformance tests | [Calling-convention conformance tests](connector-contract.md#calling-convention-conformance-tests) | For every connector, with minimal fixtures |
| Metadata conformance tests | [Metadata conformance tests](connector-contract.md#metadata-conformance-tests) | With per-source-kind fixtures |
| Data read conformance tests | [Data read conformance tests](connector-contract.md#data-read-conformance-tests) | The same |
| Query Engine adapter integration tests | Native planner callback mapping, scan-state replacement, and removal or retention of native operators | Per supported Query Engine and pinned version |
| Connector SDK planning tests | Common validation, direct planning-result construction, and rule composition | In the connector SDK |
| SQL translation tests | Direct validated-plan traversal, physical names loaded into planning metadata, dialect rendering, and typed parameters | In `connector-sql`, with a representative dialect fixture |

Every read request plan used in tests has passed the Substrait validator.

Connector conformance tests call through the connector API's C ABI only, from
`conformance`. They never call connector internals, so the connector contract
is checked at the same boundary callers use. They can prove that planning
returns `Planned` or `Unsupported`, that planned queries carry the correct
remote/local decomposition, and that remote queries execute correctly, but they
cannot observe a Query Engine's native plan.

No Query Engine Adapter is implemented in this repository yet. Each future
Adapter integration suite must exercise its pinned Query Engine planner
boundary and prove that a plan without local work replaces the offered native
region, an adoptable local plan becomes the required native work over remote
output, an unlowerable proposal and `Unsupported` change no scan state, and
execution uses the adopted executable query without changing its rewrite.

The boundary cases have explicit owners:

| Boundary case | Owning test suite |
| --- | --- |
| The initial `QueryPlan` passes through no-op rules before later rules run | Connector SDK planning tests prove rules compose and the initial plan contains one schema-matched `RemoteResult`, local filter/projection work, and no local table read |
| A rewrite rule returns an error | Connector SDK planning tests prove selection stops instead of silently falling back |
| A Connector returns another local plan | Connector SDK planning tests assert its remote/local decomposition contract; a future Adapter integration test must prove it either becomes native work over the remote result or rejects that proposal without native-state changes |
| Read, projection, and filter appear together | A future Adapter integration test must prove they are offered as one complete region rather than independent requests |
| A Spark multi-predicate callback fails after an earlier predicate was accepted | A future Spark Adapter integration test must prove that the builder rolls back to its original state |
| Trino presents the same native constraint again or adds a new exact constraint after a retained one | A future Trino Adapter integration test must prove that predicates are not appended again and every prior retained constraint remains in Trino |
| The standard DataFusion provider sees a representable filter or an unsupported projection | A future DataFusion Adapter integration test must prove that it reports filters as `Inexact`, keeps planned state local to `scan`, and supplies a local projection over a full remote scan when needed |
| A DuckDB multi-expression callback fails after an earlier expression was accepted | A future DuckDB Adapter integration test must prove that bind state and the residual vector roll back together |
| An engine creates its first connector scan | Each future Adapter integration suite must prove that it initializes the exact base `ReadRel` before later operator attempts |
| Planning facts require Data Source I/O or become invalid | Connector SDK tests prove metadata loading happens after common validation; each Connector's operation tests must prove its I/O handling and any source-specific execution guard |
| A mutable semantic fact may change after planning | Connector-specific tests prove its execution guard preserves the fact through remote evaluation, or the Connector never applies a rewrite that depends on that fact |

The last test uses an instrumented source fixture so the assertion covers the
fact for the full period in which the remote operator is evaluated. Schema or
object changes remain covered as `stale registration`; there is no common plan
invalidation protocol.

### Fixtures

Each Connector's fixtures consist of:

- a procedure that prepares the source, such as SQL that creates tables, types,
  and values, applied to a source started in a container;
- expected table metadata, excluding retrieval info and compared with the
  contract's normalization
  ([Retrieval info and content digest](connector-contract.md#retrieval-info-and-content-digest));
  and
- expected reads: per request, the expected Arrow schema and values.

### Compatibility table

Each connector publishes the tested combinations of Data Source version, ADBC driver
version (ADBC access), and platform as its compatibility table.

| Axis | Initial range |
| --- | --- |
| PostgreSQL | Major versions within community support at the time of testing |
| MySQL | The LTS version and the latest version at the time of testing |
| ADBC driver (ADBC access) | The version range the connector declares |
| Platform | The first distribution platforms of the product |

## Choosing Data Source access and ADBC drivers

The form of Data Source access is chosen by applying the rules in
[Connector API and Data Source access](connector-contract.md#connector-api-and-data-source-access)
in their stated order. ADBC-access sources follow
[Choosing an existing ADBC driver](#choosing-an-existing-adbc-driver) and
[Our own ADBC drivers](#our-own-adbc-drivers); direct-access sources follow
[Direct access](#direct-access).

### Choosing an existing ADBC driver

An existing ADBC driver is used when all of these hold:

- For each mapping rule, the values the driver returns can be turned into the
  Analytics type with value-preserving conversions. Rules for which this fails
  fall outside coverage, and the number of such rules is acceptable.
- It supports cancellation. If it does not stop the query at the source, the
  means in the connector definition can compensate.
- It keeps secret values and parameter values out of logs and errors.
- Options that change how types and values are produced can be fixed.
- Its license permits distribution, and its native dependencies can be included
  in the distribution.

These are judged through the source's conformance tests and by reviewing the
driver's documentation and implementation.

### Our own ADBC drivers

A source with no existing ADBC driver, or none that meets the conditions above,
gets our own:

- It lives in `adbc-drivers/<source>` and is built with Rust `adbc_core` and
  `adbc_ffi`.
- It handles only connection and authentication, running queries such as SQL,
  receiving results as Arrow, cancellation, and error reporting (including
  SQLSTATE and vendor codes). It does not decide types or interpret catalogs;
  the connector does.
- It converts results from the source's wire format to Arrow, choosing types
  that are already the type mapping's Analytics types to reduce conversion in
  the connector.
- It implements `AdbcErrorFromArrayStream`
  ([ADBC driver handling](connector-internals.md#adbc-driver-handling)).
- Pure Rust source clients are preferred. Keeping secret values out of logs and
  providing a means of cancellation are client selection criteria.

### Direct access

- Implement the Data Source access traits
  ([Connector trait](connector-internals.md#connector-trait)) in the connector
  crate. The Connector defines its `DataSourceQuery::Direct` request.
- Choose reader crates and source clients that return Arrow and satisfy the
  direct-access rules in
  [Data Source access rules](connector-contract.md#data-source-access-rules). Whether they
  have native dependencies is checked as in
  [Isolating native dependencies](#isolating-native-dependencies).
- Pin reader crate versions and fix the settings that change how types and
  values are produced, as in
  [Data Source access rules](connector-contract.md#data-source-access-rules); how fixed
  settings are chosen is in [Fixed settings](#fixed-settings).
- Cancellation, timeouts, and cleanup follow
  [Cancellation and timeouts](connector-internals.md#cancellation-and-timeouts)
  and [Cleanup](connector-internals.md#cleanup).

## Isolating native dependencies

The rule that native dependencies stay inside ADBC drivers
([Connector API and Data Source access](connector-contract.md#connector-api-and-data-source-access),
[Data Source access rules](connector-contract.md#data-source-access-rules)) is implemented
as follows:

- The three connector SDK crates, `connector-api`, and connector crates must
  build without system libraries. Reader crates and clients used by
  direct-access connectors are included, as dependencies of connector crates. CI
  inspects the dependency tree and fails when a crate depending on a native
  library enters them.
- TLS in our own ADBC drivers and direct-access implementations uses rustls and
  does not depend on OpenSSL.
- The licenses and origins of ADBC drivers and their native dependencies are
  managed in distribution metadata. Where drivers are placed and how they are
  bundled is owned by the product application's distribution, not by this
  repository.

## Initial PostgreSQL and MySQL conventions

| Item | PostgreSQL | MySQL |
| --- | --- | --- |
| Catalog | `pg_catalog`: columns from `pg_attribute`, Data Source types from `format_type(atttypid, atttypmod)`, type kinds from `pg_type`, constraints from `pg_constraint`, and unique indexes and partial-index conditions from `pg_index` and `pg_get_expr` | `information_schema`: columns from `COLUMNS.COLUMN_TYPE`, constraints from `TABLE_CONSTRAINTS`, `KEY_COLUMN_USAGE`, `REFERENTIAL_CONSTRAINTS`, and `CHECK_CONSTRAINTS`, and unique indexes from `STATISTICS` |
| Objects silently omitted for lack of privilege | `pg_catalog` does not omit rows by privilege, so columns are never missing. A table not found is "does not exist". | `information_schema` omits columns and tables without privilege. When table privileges cannot be confirmed, table metadata is incomplete. A table not found is "could not be read". |
| Data Source identifiers | Table OIDs and column numbers are returned | Declared as not offered |
| Data Source schema versions | Declared as not offered | Declared as not offered |
| Fixed settings | `TimeZone` to `UTC` and `client_encoding` to `UTF8`; driver options that change how types and values are produced | `time_zone` to `+00:00` and the character set to `utf8mb4`. `TIMESTAMP` values are converted by the session time zone, so fixing UTC produces `TimestampTz` values. Driver options that change how types and values are produced. |
| `Text` comparison guards | When `server_encoding` is `UTF8` and the column's Data Source type is `text` or `character varying`, `COLLATE "C"` compares bytes in code-point order. `character(n)` ignores trailing spaces, so a fully remote comparison rewrite cannot use this rule for that type. | With character set `utf8mb4`, the NO PAD binary collation `utf8mb4_0900_bin` supplies the required comparison. `utf8mb4_bin` is PAD SPACE and ignores trailing spaces, so it cannot support a fully remote comparison rewrite. Other character sets need a different proven rule. |
| Read point | Declared as not offered | Declared as not offered |
| Cancellation | The driver's cancellation; if it does not reach the source, a cancel request | The driver's cancellation; if it does not reach the source, `KILL QUERY` from another connection |
| Query time limit | `statement_timeout` | `max_execution_time` |
| Data Source-specific errors | The table in [Error classification](#error-classification) | `1045` is `authentication`; `1142` and `1143` are `permission`; `1317` (query interrupted) is `cancellation` if a cancellation was received or `timeout` if the deadline has passed; `3024` (maximum execution time exceeded) is `timeout` |

Specifying a collation in `Text` comparisons can keep the source from using an
index. A connector uses a fully remote rewrite only when the source expression
preserves the required semantics; otherwise it returns an appropriate local
plan or reports the offered region unsupported.

## Applying the conventions to other sources

The contract must be implementable beyond PostgreSQL and MySQL
([Implementability](connector-contract.md#implementability)). For connectors the
product anticipates but does not target first, this section shows how each
contract item can be met, or kept within the contract by declaring it as not
offered, and which parts of the structure it would use. These examples establish
contract implementability; they do not select a connector implementation. In
every case the contract, connector traits, and connector SDK stay the same.

### Oracle

- **Structure:** `connector-sql` with an Oracle dialect definition. Comparisons
  on `CHAR` columns and on columns with non-binary collations cannot be exact
  without a separately proven semantic rule.
- **Data Source access:** ADBC access through an ADBC driver for Oracle; without an
  existing one, our own driver using an Oracle client. Native dependencies such
  as Oracle Instant Client stay inside the driver.
- **Connection settings and credentials:** a connect descriptor, and user name
  and password as separate options.
- **Cancellation and timeouts:** the driver's cancellation, then an OCI break if
  that does not stop the query.
- **Metadata:** read from `ALL_TABLES`, `ALL_TAB_COLUMNS`, and related views.
  Data Source types and mapping are in [Oracle](type-mappings.md#oracle).
- **Constraints:** primary keys, unique constraints, foreign keys, and CHECK
  constraints.
- **Identifiers and schema versions:** object IDs.
- **Missing information:** objects hidden by privilege are "could not be read".
- **Data read:** read consistency within one query; the SCN can be the read
  point.
- **Filter planning:** apply a remote filter rewrite only when the Oracle type
  and collation semantics are proven equivalent. A rule may instead return a
  semantics-preserving local filter over remote output; the Adapter decides
  whether that proposal can be installed.

### ScalarDB

- **Structure:** no `connector-sql`; catalog, planner, and executor are written
  in code, translating final remote `LogicalPlan` values to ScalarDB scans.
- **Data Source access:** both direct gRPC access and an ADBC driver can satisfy the
  common contract. Direct access keeps the Rust gRPC client in the connector;
  ADBC access keeps it inside the driver. Any JVM implementation remains subject
  to the exception boundary in
  [Implementation form](connector-internals.md#implementation-form).
- **Connection settings and credentials:** the ScalarDB Cluster endpoint and
  authentication information.
- **Cancellation and timeouts:** gRPC call cancellation, the same in every form.
- **Metadata:** ScalarDB namespace and table metadata. Data Source types are
  ScalarDB type names, which need a new type mapping.
- **Constraints:** only primary keys (partition key and clustering key); others
  declared as not offered.
- **Identifiers and schema versions:** declared as not offered.
- **Missing information:** objects hidden by privilege are "could not be read".
- **Data read:** read by scan; the read point is declared as not offered.
- **Filter planning:** filters expressible with equivalent ScalarDB scan
  semantics can execute remotely. A useful but incomplete scan may be proposed
  with an exact local filter over its output; the Adapter decides whether that
  proposal can be installed.
  ScalarDB does not speak SQL, so catalog reading and plan translation are
  connector code.

### Generic ODBC

- **Structure:** `connector-sql`, with dialect and catalog queries written as a
  per-Connector definition.
- **Data Source access:** ADBC access through an ADBC driver that uses ODBC. The ODBC
  driver manager and the source's ODBC driver stay inside that driver.
- **Connection settings and credentials:** the ODBC connection string without
  secrets, and secrets as separate options.
- **Cancellation and timeouts:** the driver's cancellation (ODBC `SQLCancel`).
- **Metadata:** ODBC catalog functions, or catalog queries written in the
  dialect table. Data Source types and mapping follow
  [Support tiers](connector-contract.md#support-tiers).
- **Constraints:** primary keys, foreign keys, and unique indexes available
  through ODBC catalog functions; CHECK constraints declared as not offered.
- **Identifiers and schema versions:** declared as not offered.
- **Missing information:** some ODBC drivers' catalog functions silently omit
  objects by privilege, so table metadata is incomplete unless the omission can
  be ruled out.
- **Data read:** the read point is declared as not offered.
- **Filter planning:** decided for the concrete ODBC source and driver context;
  the dialect table only supplies translation data and conservative guards.

### Query Engine-backed (Trino)

- **Structure:** `connector-sql` with a Trino dialect definition; namespaces
  start with the catalog.
- **Data Source access:** ADBC access through an ADBC driver for Trino; without an
  existing one, our own.
- **Connection settings and credentials:** the Trino endpoint and
  authentication information.
- **Cancellation and timeouts:** Trino's query cancellation API.
- **Metadata:** Trino's `information_schema`, with a catalog level. Its connector
  owns the mapping from Trino Data Source types to Analytics types.
- **Constraints, identifiers, and schema versions:** declared as not offered.
- **Missing information:** when Trino silently omits objects by privilege, table
  metadata is incomplete.
- **Data read:** the read point is declared as not offered.
- **Filter planning:** translated to Trino SQL only after the Connector checks
  the complete offered plan against Trino type and session semantics.

### Iceberg (direct access)

- **Structure:** only `connector-sdk`; `catalog`, `translator`, and the source
  access traits are written in code.
- **Data Source access:** direct access through `iceberg-rust`, reading table-format
  metadata and data files, with no native dependencies.
- **Connection settings and credentials:** a table-format catalog, such as an
  Iceberg REST catalog, or a registered location, plus object storage
  credentials, with secrets as separate options.
- **Cancellation and timeouts:** the connector stops data file reads and calls
  to the catalog and object storage in progress.
- **Metadata:** the schema in the table metadata. Data Source types and mapping are
  in [Iceberg](type-mappings.md#iceberg).
- **Constraints:** no enforced constraints. The schema's identifier fields can
  be returned as a declared-only, unenforced primary key
  ([Constraints](connector-contract.md#constraints)).
- **Identifiers and schema versions:** the table UUID, field IDs, and the
  current schema ID
  ([Data Source identifiers and schema versions](connector-contract.md#data-source-identifiers-and-schema-versions)).
- **Missing information:** objects hidden by catalog privileges are "could not
  be read".
- **Data read:** one snapshot is read, so one read request reads one
  point-in-time state of the table; the snapshot ID can be the read point.
- **Filter planning:** partition and column statistics may drive data-file
  skipping, but skipping alone is not exact row filtering. Such pruning is
  represented by a remote pruning plan plus a local row-level filter. Omitting
  the local filter requires a remote row-level implementation with equivalent
  semantics.
