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
3. Create `crates/connectors/<source>` and implement the
   [connector trait](connector-internals.md#connector-trait) in Rust code and
   constant tables ([Connector behavior as code](connector-internals.md#connector-behavior-as-code)).
   Every connector implements `Connector`, which names its components, its
   error handler, and its query, and declares its descriptor and value
   conversion. A source that speaks SQL through ADBC also implements
   `AdbcConnector` and `SqlConnector`, which declare its driver, options,
   catalog reads, planning facts, semantic decisions, and dialect;
   `connector-adbc` and `connector-sql` supply the access, catalog, planner,
   and executor components from them, and the SDK runs the planning and read
   flows. An ADBC source that does not speak SQL implements `AdbcConnector`
   and supplies its own catalog, planner, and executor. For direct access, the
   connector supplies all of its components and reads the Data Source itself
   ([Direct access](#direct-access)).
4. Fill `ConnectorDescriptor` with the name, version, Connectors, the
   description of accepted options
   ([Static information](connector-contract.md#static-information)), the form
   of Data Source access, and, with ADBC access, the ADBC driver.
5. Write the connector's fixture for the conformance suite in
   `crates/connectors/<source>/tests` and pass the suite for this connector
   alone ([Tests](#tests)).
6. Register it as a `connector-api` feature and build it into the artifact.
   Run the [dependency policy check](#dependency-policy-check) to confirm that
   the connector brings in no native code, no ADBC driver loading of its own,
   and no license that prevents distribution.

These steps change neither the connector SDK, the contract, nor the callers of
the connector API; `connector-api` only gains a feature, without code changes
([Build cycle and versions](connector-internals.md#build-cycle-and-versions)).

## Retrieving metadata

### Stages

Table metadata is produced in these stages:

| Stage | Performed by | Input | Output |
| --- | --- | --- | --- |
| 1. Read the catalog | The connector, running its catalog queries (with `connector-sql` for SQL sources) or its own code | A set of tables | The connector's source metadata: names, ordinals, nullability, Data Source types, constraints, identifiers, and missing items, plus whatever else the connector reads |
| 2. Parse Data Source types | The connector | Data Source types, and catalog information needed to tell type kinds apart | Keys for looking up the type mapping |
| 3. Apply the type mapping | The connector, in `to_table_metadata` | Keys and the connector's coverage | Analytics types, or unsupported with a reason |
| 4. Build table metadata | The connector, through the SDK's `TableMetadata::new` | Results of stages 1 to 3 | Table metadata in deterministic order, with completeness derived from its values |
| 5. Return the result | The connector SDK runtime, then the connector API | Table metadata, source metadata, and retrieval info | The runtime's `TableMetadataResult`, converted by the connector API to the C structures of the table metadata result |

The type mapping needs source knowledge, so the connector applies it. What
every Connector shares is the shape of the result: the SDK's column types
cannot state an Analytics type without the Data Source type it came from, and
`TableMetadata::new` fixes the order of columns and constraints.

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
  connector SDK repeats the query per namespace; the connector only supplies
  the per-namespace query.
- The retrieval time is the time each table's catalog was read.

### Retrieval info

The connector SDK takes the following retrieval info items from the
connector's `ConnectorDescriptor` and, with ADBC access, the ADBC driver's
report:

- The connector's name and version and, with ADBC access, the ADBC driver's name
  and version. Direct-access reader crates and fixed settings are identified by
  the connector version
  ([Retrieval info and determinism](connector-contract.md#retrieval-info-and-determinism)).
- The library version
  ([Compatibility and versions](connector-contract.md#compatibility-and-versions)).

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

- The connector's constant tables list, per mapping rule, the Arrow type the
  Data Source access (ADBC driver or reader crate) returns and the building block that
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

Two kinds of settings that change how types and values are produced are fixed
by the connector, and changing them raises the connector's version
([Data Source types](connector-contract.md#data-source-types)):

- session settings run right after connecting, such as the time zone; and
- ADBC driver options or reader crate settings that change how types and values
  are produced.

Connection settings passed by the caller cannot override fixed settings.

## Reading data

### Translation

- Filter literals in the remote Substrait plan are rendered directly into one
  non-parameterized query. `LogicalPlan` owns their validation and exposes each
  as a canonical `LiteralValue`; the SQL dialect owns every source-specific
  spelling and escape rule. Common translation never concatenates an
  unrendered runtime string into SQL.
- The generated SQL text contains literal values and is sensitive. Connector
  logs, connector-composed error messages, and typed diagnostics never include
  that text. A separately returned Data Source message may contain user data as
  defined by the connector contract and remains restricted from ordinary logs.
  `LogicalPlan` debug output redacts literal expressions. Raw pushdown requests,
  planned pushdowns, and executable queries intentionally do not implement
  `Debug` because they contain literal-bearing boundary plans or
  Connector-owned executable queries.
- One read request becomes one query to the source. Splitting it into several
  queries would break the rule that one request reads a single point-in-time
  state ([Consistency](connector-contract.md#consistency)).
  Auxiliary queries that read attributes or types from the catalog for planning
  or for the schema check
  ([Plan-aware support decisions](#plan-aware-support-decisions),
  [Type information for the schema check](#type-information-for-the-schema-check))
  read no rows and are outside this rule.
- Planning constructs the query that implements the remote plan; with the
  remote output schema it forms the `RemoteExecution`. The query type
  implements `PortableQuery` so that the `RemoteExecution` can be carried to
  another process.
  Execution sends that generated query directly rather than retranslating the
  Substrait plan. The Connector's planner and executor share its concrete
  query type, `Connector::Query`. The SDK's planned read reads the table's
  current metadata through the Connector's `Catalog` for the schema check, has
  its `Executor` send the query, and converts each batch with
  `Connector::convert_column`. For the shared SQL path, the query is `SqlRead`,
  whose SQL text `connector-sql::SqlExecutor` passes directly to ADBC. When work remains
  local, the Connector returns a `local_plan` as part of its proposal. The
  Adapter adopts the proposal only if it can convert the complete local plan to
  native Query Engine work. The connector SDK does not apply a leftover
  expression after the source returns Arrow.
- Limit, aggregation, sort, TopN, and join translation are not part of the
  initial implementation. Their planner rules stay `Unsupported` until their
  end-to-end semantics and execution paths exist.

### Plan-aware support decisions

The Connector's `Planner` component defines a private facts type, loads the
source facts needed by the offered `LogicalPlan`, supplies an ordered
`RewriteRuleSet<Facts>` over `LogicalPlan`, and generates the executable query
from the final remote logical plan. The SDK's `Connector::plan` implements the
common flow around those three operations, including constructing
`LogicalPlan` while validating Substrait and converting the result back to
Substrait.
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
an appropriate local plan. Query generation may instead return
`Unsupported`; planning must never produce a silently different decomposition.

The source attributes used for the decision, such as Data Source type, collation,
character set, and Data Source version, must be valid for the planned execution.
After common validation, Connector planning interprets opaque table bindings and
reads the catalog or source facts required by that complete region. This operation
may perform timeout-bound, cancellable Data Source I/O. A Connector may prefetch
or cache facts for a session, but the cache is an implementation strategy rather
than a required planning-snapshot abstraction.

Those source attributes are treated as unchanged when the planned query runs;
the connector does not recheck them at execution, and the result is not
guaranteed if they change
([Read request](connector-contract.md#read-request)). Result type information
is still used by the schema check, but information available only after the
data query starts cannot justify an earlier rewrite decision.

Semantic guard tables are managed with the connector's version. Data-read
conformance tests use fixtures whose comparison meaning varies to prove fully
remote rewrites, rewrites with local work, and unsupported outcomes.

### Type information for the schema check

The connector SDK obtains the Data Source types of a query's result in the same form
as catalog Data Source types and uses them for the
[schema check](connector-contract.md#schema-check):

- With ADBC access, when the ADBC driver puts the Data Source type name in the
  result's Arrow field metadata, that is used. The connector names the
  metadata key.
- With direct access, the type is obtained, in the same form as the catalog
  Data Source type, from the schema the reader crate read at read time, such as
  table-format metadata or a file footer.
- When the driver does not provide type names, or the result's type information
  cannot identify the mapping rule (PostgreSQL domains and enums, MySQL `ENUM`),
  the column's type is read from the catalog before the query.

## Errors, logs, and metrics

### Error classification

The connector SDK classifies Data Source access errors into the contract's
[error kinds](connector-contract.md#errors). Direct-access errors are
classified by the Connector's error handler (`Connector::ErrorHandler`) and
its access's generic classes. The rest of this section covers
ADBC access. ADBC errors carry a status code, a SQLSTATE, and a Data Source-specific
vendor code. Classification by vendor code (the connector's error table) is
applied before classification by SQLSTATE class; for example, MySQL `1142`
returns SQLSTATE `42000` but is classified as `permission`.

| SQLSTATE (class or code) | Kind | Classified in |
| --- | --- | --- |
| `08` (connection exception) | `connection` | `connector-adbc` |
| `28` (invalid authorization specification) | `authentication` | `connector-adbc` |
| `22` (data exception) | `data-source`; `internal` when caused by an invalid literal representation generated by the connector | `connector-adbc`, and the connector SDK, which judges the cause |
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
- Filter literals and generated SQL text are never fields of log events.
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

That secrets and literals do not leak through the connector is checked by the
conformance suite ([What the suite checks](#what-the-suite-checks)).

## Tests

A connector shows that it meets the
[connector contract](connector-contract.md) by passing the conformance suite,
which the connector SDK provides and which knows no Connector, with a fixture
of its own. Behavior that does not depend on the Connector is implemented
once, in the connector SDK crates and `connector-api`, and is tested once
there; the suite does not repeat it.

### Test kinds

| Test | Checks | Where it runs |
| --- | --- | --- |
| Conformance suite | The Connector-dependent part of the contract's [calling-convention](connector-contract.md#calling-convention-conformance-tests), [metadata](connector-contract.md#metadata-conformance-tests), and [data read](connector-contract.md#data-read-conformance-tests) conformance tests ([What the suite checks](#what-the-suite-checks)) | The `connector-sdk` `conformance` feature, run from `crates/connectors/<source>/tests` with the connector's fixture against a source started in a container |
| C ABI tests | Unit lifetimes and release order at the C boundary, requests while a read stream is open, cancellations and timeouts passed through the C ABI, retrieving an operation's error after a failed pull, restoring a remote execution in another session, and converting metadata and comparisons to and from C structures for every Analytics type | In `connector-api`, with the SDK's in-memory test Connector where a Connector is needed |
| Connector SDK runtime tests | Dispatch by Connector name, option validation, the session lease and usability, lazy reads, cancellation, the timeout watcher, panic containment, the schema check, and secret-safe errors | In the connector SDK, with its in-memory test Connector |
| Connector SDK planning tests | Common validation, direct planning-result construction, and rule composition | In the connector SDK |
| ADBC access tests | The query lifecycle with ADBC (no source I/O before the first pull, cancellation before and during execution and pulls, releasing the reader), the driver version check, and error classification by ADBC status and SQLSTATE class | In `connector-adbc` |
| SQL translation tests | Direct validated-plan traversal, physical names loaded into planning metadata, and delegation of identifier and literal rendering | In `connector-sql`, with a source-neutral diagnostic dialect fixture |
| Query Engine adapter integration tests | Native planner callback mapping, scan-state replacement, and removal or retention of native operators ([Integration tests](adapter-implementation.md#integration-tests)) | Per supported Query Engine and pinned version |

Every read request plan used in tests has passed the Substrait validator.

The suite calls the connector through the SDK runtime, as `connector-api`
does, with only that connector built into the library. It never calls the
connector trait or the connector's private functions directly, so session
leases, cancellations, and timeouts behave as callers see them. It does not go
through the C ABI, which adds no Connector-dependent behavior: the C ABI tests
check its metadata conversions for every Analytics type, and read results
cross it unchanged through the Arrow C stream interface, so calling a real
connector through it would check nothing more, and the connector crate runs
the suite without building `connector-api`
([Build cycle and versions](connector-internals.md#build-cycle-and-versions)).
The suite can prove that planning returns `Planned` or `Unsupported`, that
planned queries carry the correct remote/local decomposition, and that remote
queries execute correctly, but it cannot observe a Query Engine's native plan.

What happens to a planned result inside a Query Engine is tested by each
Adapter's [integration suite](adapter-implementation.md#integration-tests).

The boundary cases on the connector side have explicit owners:

| Boundary case | Owning test suite |
| --- | --- |
| The initial `QueryPlan` passes through no-op rules before later rules run | Connector SDK planning tests prove rules compose and the initial plan contains one schema-matched `RemoteResult`, local filter/projection work, and no local table read |
| A rewrite rule returns an error | Connector SDK planning tests prove selection stops instead of silently falling back |
| A Connector returns another local plan | Connector SDK planning tests assert its remote/local decomposition contract; the Adapter integration suite covers lowering or rejecting it |
| Planning facts require Data Source I/O | Connector SDK tests prove metadata loading happens after common validation; each Connector's operation tests must prove its I/O handling |

Schema or object changes are covered as `stale registration`; there is no
common plan invalidation protocol.

### What the suite checks

The suite checks the following against the fixture's source:

- **Metadata retrieval.** Every mapping rule of its type mapping returns the
  expected Analytics type, unmapped types and uncovered rules return
  unsupported columns, and names, ordinals, nullability, Data Source types,
  constraints, source identifiers, and missing information match the
  expectations. The fixtures include several namespaces, names that differ
  only in case, targets without privilege, and nonexistent tables. Name
  filters match exactly, and retrieving the same tables twice returns the same
  table metadata apart from retrieval info
  ([Metadata conformance tests](connector-contract.md#metadata-conformance-tests)).
- **Values.** Each covered rule's boundary values (minimum, maximum, precision
  limit, time zones, and NULL) read as the expected Arrow types and values.
  Projecting a column with a value that cannot be carried exactly is a `data`
  error; not projecting it is not.
- **Pushdown.** Each read case plans as the fixture states, fully remote, with
  local work, or unsupported, and its remote result returns the stated rows;
  a fully remote result returns the rows the offered region would. The
  fixture covers every rewrite rule with values whose comparison meaning
  varies by column, such as trailing spaces, letter case, accents, and
  character sets, and with non-normalized expression trees.
- **Schema check.** Reading after changing a column's type, dropping a
  referenced column or table, or making a NOT NULL column nullable at the
  source is `stale registration`; making a nullable column NOT NULL is not
  ([Data read conformance tests](connector-contract.md#data-read-conformance-tests)).
- **Stopping at the source.** Cancellation, timeout, and releasing a stream
  partway stop the query at the source, for metadata requests and reads. A
  broken source connection makes the session reject the next request as
  `invalid state`.
- **Errors.** Each failing request the fixture states returns its kind and
  retry flag. No error the suite receives reveals, in its message, Data
  Source message, or diagnostics, a value of an option the connector's
  descriptor marks secret or a string literal of its request's filter.

Every extension-encoded type a connector can return is registered, because
the Analytics type model is a closed set of types, so the suite does not
check registration.

The rest of the contract's calling-convention conformance tests, such as
release order, cancellations given before a request, and retrieving errors
mid-stream, do not depend on the Connector and belong to the C ABI, SDK
runtime, and ADBC access tests.

A behavior a connector does not yet meet is stated in its fixture as a known
deviation, with what the connector does instead, and recorded in its README's
known limitations. The suite checks the deviating behavior as stated and fails
once the connector meets the contract, so a fix also updates the fixture and
the README.

### Fixtures

Each connector's fixture implements the suite's `Fixture` trait in
`crates/connectors/<source>/tests` and consists of:

- a procedure that prepares the source, such as SQL that creates tables, types,
  and values, applied to a source started in a container;
- metadata requests with their expected results: lists, and table metadata
  excluding retrieval info and in the contract's deterministic order
  ([Retrieval info and determinism](connector-contract.md#retrieval-info-and-determinism));
- read cases: per offered region, its planning outcome and the expected values
  or error of its remote result;
- source changes between planning and reading, and failing requests with the
  credential each uses; and
- the source-side operations the suite cannot perform through the contract:
  a read that waits at the source, a read that outlasts its first batch,
  making metadata requests wait, observing whether the fixture's queries run,
  and breaking the fixture's connections.

### Compatibility table

Each connector publishes, in its README, the combinations of Data Source
version, ADBC driver version (ADBC access), and platform that passed its
conformance tests as its compatibility table.

| Axis | Initial range |
| --- | --- |
| PostgreSQL | Major versions within community support at the time of testing |
| MySQL | The LTS version and the latest version at the time of testing |
| ADBC driver (ADBC access) | The supported version range in the connector's static information |
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
- Its cancellation stops the query at the source.
- It does not copy the literal-bearing SQL text into logs or errors.
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
  cancellation that stops the query at the source are client selection
  criteria.

### Direct access

- Implement the [`Connector` trait](connector-internals.md#connector-trait)
  and its components in the connector crate, reading the Data Source directly
  inside them.
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
  direct-access connectors are included, as dependencies of connector crates.
  CI enforces this with the [dependency policy check](#dependency-policy-check).
- TLS in our own ADBC drivers and direct-access implementations uses rustls and
  does not depend on OpenSSL.
- The licenses and origins of ADBC drivers and their native dependencies are
  managed in distribution metadata. Where drivers are placed and how they are
  bundled is owned by the product application's distribution, not by this
  repository.

### Dependency policy check

`deny.toml` at the repository root states the policy for the dependencies that
ship in the library, and `cargo deny check bans licenses` enforces it in CI.
Run it before submitting a connector:

```sh
nix develop --command cargo deny check bans licenses
```

The check inspects every `connector-api` feature for the Linux and macOS
targets, so it also reports dependencies that appear only on Linux. Development
and test dependencies are not inspected. It fails in the following cases:

| Failure | Cause | Usual resolution |
| --- | --- | --- |
| A crate that compiles or links native code is banned (`cc`, `cmake`, `pkg-config`, `bindgen`, `vcpkg`, `openssl-sys`) | A source client or reader crate builds a native library. A common path is a client's default `native-tls` feature, which links OpenSSL on Linux but uses the system framework on macOS. | Disable the client's default features and select its rustls feature, or choose a pure Rust client. With ADBC access, the native code belongs in the ADBC driver. |
| `adbc_driver_manager` is banned for the connector crate | The connector loads ADBC drivers itself instead of through `connector-adbc`. | Use the access component that `connector-adbc` supplies ([ADBC driver handling](connector-internals.md#adbc-driver-handling)). |
| A license is rejected | A dependency's license is not in the allowed list. | Choose another crate, or add the license to `allow` after confirming that it permits distributing the library. |

A connector that intentionally accepts a dependency outside the policy, such as
a private in-house connector that uses a GPL library or a native client,
records an explicit exception in `deny.toml` so that it is visible in review.
The exception covers only the named crate, and the policy keeps applying to
every other crate:

```toml
[licenses]
exceptions = [{ crate = "<dependency>", allow = ["GPL-3.0-only"] }]

[bans]
deny = [
    # `wrappers` lists the crates allowed to depend on the banned crate directly.
    { crate = "cc", wrappers = ["<dependency>"], reason = "..." },
]
```

## Initial PostgreSQL and MySQL conventions

| Item | PostgreSQL | MySQL |
| --- | --- | --- |
| Catalog | `pg_catalog`: columns from `pg_attribute`, Data Source types from `format_type(atttypid, atttypmod)`, type kinds from `pg_type`, constraints from `pg_constraint`, and unique indexes and partial-index conditions from `pg_index` and `pg_get_expr` | `information_schema`: columns from `COLUMNS.COLUMN_TYPE`, constraints from `TABLE_CONSTRAINTS`, `KEY_COLUMN_USAGE`, `REFERENTIAL_CONSTRAINTS`, and `CHECK_CONSTRAINTS`, and unique indexes from `STATISTICS` |
| Objects silently omitted for lack of privilege | `pg_catalog` does not omit rows by privilege, so columns are never missing. A table not found is "does not exist". | `information_schema` omits columns and tables without privilege. When table privileges cannot be confirmed, table metadata is incomplete. A table not found is "could not be read". |
| Data Source identifiers | Table OIDs and column numbers are returned | Not offered |
| Fixed settings | `TimeZone` to `UTC`, `client_encoding` to `UTF8`, and `standard_conforming_strings` to `on` for generated literals; driver options that change how types and values are produced. `search_path` is a connection option rather than a fixed setting: it changes only whether `format_type` qualifies a type with its schema, and generated SQL qualifies every table. | `time_zone` to `+00:00` and the character set to `utf8mb4`. `TIMESTAMP` values are converted by the session time zone, so fixing UTC produces `TimestampTz` values. Driver options that change how types and values are produced. |
| `Text` comparison guards | Equality on a `text` or `character varying` column with a deterministic collation compares bytes, so it runs remotely. When `server_encoding` is `UTF8`, `COLLATE "C"` would also compare bytes in code-point order for ordering; the initial connector does not render collations, so ordering stays local. `character(n)` ignores trailing spaces, so a fully remote comparison rewrite cannot use either rule for that type. | With character set `utf8mb4`, the NO PAD binary collation `utf8mb4_0900_bin` supplies the required comparison. `utf8mb4_bin` is PAD SPACE and ignores trailing spaces, so it cannot support a fully remote comparison rewrite. Other character sets need a different proven rule. |
| Cancellation | The driver's cancellation, which must stop the query at the source | The driver's cancellation, which must stop the query at the source |
| Data Source-specific errors | The table in [Error classification](#error-classification) | `1045` is `authentication`; `1142` and `1143` are `permission`; `1317` (query interrupted) is `cancellation` if a cancellation was received or `timeout` if the deadline has passed; `3024` (maximum execution time exceeded) is `timeout` |

Specifying a collation in `Text` comparisons can keep the source from using an
index. A connector uses a fully remote rewrite only when the source expression
preserves the required semantics; otherwise it returns an appropriate local
plan or reports the offered region unsupported.

## Applying the conventions to other sources

The contract must be implementable beyond PostgreSQL and MySQL
([Implementability](connector-contract.md#implementability)). For connectors the
product anticipates but does not target first, this section shows how each
contract item can be met, or kept within the contract by recording it as not
offered, and which parts of the structure it would use. These examples establish
contract implementability; they do not select a connector implementation. In
every case the contract, connector traits, and connector SDK stay the same.

### Oracle

- **Structure:** `connector-sql` with an Oracle SQL dialect. Comparisons
  on `CHAR` columns and on columns with non-binary collations cannot be exact
  without a separately proven semantic rule.
- **Data Source access:** ADBC access through an ADBC driver for Oracle; without an
  existing one, our own driver using an Oracle client. Native dependencies such
  as Oracle Instant Client stay inside the driver.
- **Connection settings and credentials:** a connect descriptor, and user name
  and password as separate options.
- **Cancellation and timeouts:** the driver's cancellation, which must stop the
  query at the source.
- **Metadata:** read from `ALL_TABLES`, `ALL_TAB_COLUMNS`, and related views.
  Data Source types and mapping are in [Oracle](type-mappings.md#oracle).
- **Constraints:** primary keys, unique constraints, foreign keys, and CHECK
  constraints.
- **Identifiers:** object IDs.
- **Missing information:** objects hidden by privilege are "could not be read".
- **Data read:** read consistency within one query.
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
  not offered.
- **Identifiers:** not offered.
- **Missing information:** objects hidden by privilege are "could not be read".
- **Data read:** read by scan.
- **Filter planning:** filters expressible with equivalent ScalarDB scan
  semantics can execute remotely. A useful but incomplete scan may be proposed
  with an exact local filter over its output; the Adapter decides whether that
  proposal can be installed.
  ScalarDB does not speak SQL, so catalog reading and plan translation are
  connector code.

### Generic ODBC

- **Structure:** `connector-sql`, with dialect and catalog queries written per
  Connector as constant tables.
- **Data Source access:** ADBC access through an ADBC driver that uses ODBC. The ODBC
  driver manager and the source's ODBC driver stay inside that driver.
- **Connection settings and credentials:** the ODBC connection string without
  secrets, and secrets as separate options.
- **Cancellation and timeouts:** the driver's cancellation (ODBC `SQLCancel`).
- **Metadata:** ODBC catalog functions, or catalog queries written in the
  dialect table. Data Source types and mapping follow
  [Support tiers](connector-contract.md#support-tiers).
- **Constraints:** primary keys, foreign keys, and unique indexes available
  through ODBC catalog functions; CHECK constraints not offered.
- **Identifiers:** not offered.
- **Missing information:** some ODBC drivers' catalog functions silently omit
  objects by privilege, so table metadata is incomplete unless the omission can
  be ruled out.
- **Filter planning:** decided for the concrete ODBC source and driver context;
  the dialect table only supplies translation data and conservative guards.

### Trino

- **Structure:** `connector-sql` with a Trino SQL dialect; namespaces
  start with the catalog.
- **Data Source access:** ADBC access through an ADBC driver for Trino; without an
  existing one, our own.
- **Connection settings and credentials:** the Trino endpoint and
  authentication information.
- **Cancellation and timeouts:** Trino's query cancellation API.
- **Metadata:** Trino's `information_schema`, with a catalog level. Its connector
  owns the mapping from Trino Data Source types to Analytics types.
- **Constraints and identifiers:** not offered.
- **Missing information:** when Trino silently omits objects by privilege, table
  metadata is incomplete.
- **Filter planning:** translated to Trino SQL only after the Connector checks
  the complete offered plan against Trino type and session semantics.

### Iceberg (direct access)

- **Structure:** only `connector-sdk`; `catalog`, planner, and executor are
  written in code and read the source directly.
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
- **Identifiers:** the table UUID and field IDs
  ([Data Source identifiers](connector-contract.md#data-source-identifiers)).
- **Missing information:** objects hidden by catalog privileges are "could not
  be read".
- **Data read:** one snapshot is read, so one read request reads one
  point-in-time state of the table.
- **Filter planning:** partition and column statistics may drive data-file
  skipping, but skipping alone is not exact row filtering. Such pruning is
  represented by a remote pruning plan plus a local row-level filter. Omitting
  the local filter requires a remote row-level implementation with equivalent
  semantics.
