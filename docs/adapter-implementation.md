# Query Engine adapter implementation conventions

## Summary

This document defines the conventions for implementing one Query Engine
adapter on the connector API: how an adapter uses the C API, how it builds
offered plans, how it applies a planned result, how it maps types, and how it
is tested. What the connector API promises is defined by the
[connector contract](connector-contract.md); what an adapter is responsible for
and which native planner hooks it can use are defined in
[Query Engine pushdown adapters](query-engine-pushdown.md). This document
defines how to meet them. Terms follow the
[pushdown glossary](pushdown-planning.md#glossary).

The library and its adapters are updated together
([Adding Analytics types](analytics-types.md#adding-analytics-types)), so the
Substrait profile in the contract is a requirement on adapters, not a profile
negotiated at run time. An adapter builds plans that meet the profile and
checks them in its tests by passing them to `plan_pushdown`, which validates
every offered plan ([Tests](#tests)).

## Using the connector API

The C API is declared in
`crates/connector-api/include/scalardb_analytics_connector.h`. The header's
opening comment states the conventions for errors, text, bytes, input copies,
and timeouts; each function's comment states the lifetime of what it returns.
The library exports only `scalardb_analytics_connector_api`, which returns the
table of every other function; the function names below are that table's
members.

### Units an adapter holds

Who initializes the library depends on the process
([Units and concurrency](connector-contract.md#units-and-concurrency)):

- **In a Query Engine's process**, the usual case, the adapter owns the library.
  It calls `library_initialize` when the Query Engine loads it, with the ADBC
  driver location and logging settings taken from the Query Engine's
  configuration. It calls `library_shutdown` only when the Query Engine unloads
  it, after closing its data sources, and not at all if the Query Engine never
  unloads it.
- **When an application embeds the Query Engine**, the application owns the
  library and the adapter receives it initialized. The adapter then never calls
  `library_initialize` or `library_shutdown`.

The caller always knows which case it is in, so an adapter exposes the two as
separate entry points: the one the Query Engine loads as a plugin initializes
the library, and the one an embedding application uses does not.

The adapter opens one data source per registered data source and as many
sessions as it needs concurrent planning calls and reads.

A session serves one call at a time. Both `plan_pushdown` and a read hold the
session: planning on a session whose read stream has not been fully pulled or
released returns `invalid state`, and so does starting a read on it. An adapter
that plans while another query reads therefore plans on another session. A
session, its operations, and its streams are used from one thread at a time;
only `operation_cancel` may be called from any thread.

Children are released before their parents: streams and operations before their
session, sessions before their data source. The library never touches freed
memory when this order is broken, but calls on the orphaned child return
`invalid state`.

### Planning

One optimizer attempt for one native region is one planning call:

~~~text
plan_pushdown(session, offered plan bytes) -> pushdown result
  pushdown_result_is_planned
    false: pushdown_result_unsupported_reason        keep the native plan
    true:  pushdown_result_remote_plan                Substrait bytes
           pushdown_result_local_plan                 Substrait bytes, absent for complete pushdown
           pushdown_result_remote_execution           handle the adapter keeps if it adopts the result
pushdown_result_free
~~~

The plan bytes are borrowed from the pushdown result, so the adapter decodes
them before freeing it. The remote execution handle is independent of the
result and is freed with `remote_execution_free` when the proposal is rejected
or the scan state that holds it is dropped.

`Unsupported` is a normal planning outcome. An error from `plan_pushdown` also
leaves the native plan unchanged; `invalid request` means that the offered plan
is outside the contract, which is an adapter defect. Planning may query the
Data Source from inside the optimizer callback, so an adapter does not treat it
as an in-memory lookup; the library bounds it with its own fixed timeout, and
`timeout` is one of the errors that leave the native plan unchanged
([Optimizer-time Data Source I/O](query-engine-pushdown.md#optimizer-time-data-source-io)).

### Reading

An adopted result is executed by `read_start`, which takes a session, the
remote execution, and a timeout and returns an operation and an
`ArrowArrayStream`. The query is sent at the first pull and the timeout counts
from there, so an adapter can start the read
while it builds its execution state without the timeout running. Every batch
has the remote output schema; the adapter maps it to Query Engine types
([Types](#types)) and runs the local plan, if any, over it.

The read is complete only when the stream has been pulled to the end. When a
pull fails, the adapter retrieves the structured error with `operation_error`
before closing the operation; the stream itself carries only an error number
and text
([Errors in the API](connector-contract.md#errors-in-the-api)). Releasing the
stream early stops the source query and frees the session for the next call. A
`stale registration` error does not replan the running execution; only a retry
of the query by its caller does
([Functions](connector-contract.md#functions)).

A remote execution is read on any session of a data source of the Connector
that planned it; a session of another Connector returns `invalid request`.

### Execution in another process

When the Query Engine plans and executes in different processes, such as a
coordinator and its workers, the planning process encodes the adopted remote
execution with `remote_execution_encode`, frees the returned bytes with
`owned_bytes_free` after copying them, and the executing process restores the
execution with `remote_execution_restore` on its own session. The bytes are
opaque, contain no credentials, and are valid only for the same library version
([Execution in another process](pushdown-planning.md#execution-in-another-process)).
The remote execution handle itself is a process-local pointer, so the
adapter's class that the Query Engine serializes to its workers, such as a
Spark input partition or a Trino split, carries these bytes as a field. The
Query Engine owns the serialization; the adapter never interprets the bytes.

## Building offered plans

An adapter builds offered plans with its own language's Substrait library, such
as the generated protobuf types, and passes the serialized `Plan`. The
connector API accepts nothing but Substrait at this boundary
([Separate interchange and planning representations](design/separate-interchange-and-planning-representations.md)).
The accepted shape, the type encoding, and the extension declarations are
defined in [Read request representation](connector-contract.md#read-request-representation).

An adapter follows these conventions when it translates a native region:

- **One region, one complete request.** Read, filter, and projection that the
  native planner presents together are offered as one plan. Alternative regions
  or join topologies are separate requests
  ([Alternative requests and joins](query-engine-pushdown.md#alternative-requests-and-joins)).
- **The binding is copied, not built.** `namedTable` carries the table binding
  exactly as the Physical Catalog stores it. The adapter never derives it from
  source names, because only the Connector interprets it.
- **The base schema is the registered schema.** `baseSchema` encodes the
  Analytics types and nullability that the Physical Catalog registered for the
  table, with every nullability explicit. Unsupported columns are not part of
  it, so a native region that uses one is not offered
  ([Relation to table metadata and unsupported columns](connector-contract.md#relation-to-table-metadata-and-unsupported-columns)).
- **Scan-local hooks become explicit operators.** When a Query Engine hands the
  adapter pushed filters or projected columns at the scan, as DuckDB table
  functions do, the adapter expresses them as `FilterRel` and `ProjectRel` above
  a bare `ReadRel`, never in `ReadRel.filter` or `ReadRel.projection`.
- **Only representable semantics are offered.** Native parameters are resolved
  to literals first. A condition whose meaning or literal Substrait cannot
  express stays in the Query Engine and is not part of the offered region.
- **Unknown functions stay local by declaration.** A native function outside the
  profile may be declared under its own URI. The operator containing it then
  stays in the local plan, so the adapter must be able to lower that function
  in a local plan it receives.

## Applying a planned result

The remote plan describes the work the Connector executes; the adapter does not
lower it. It checks the remote output against the columns the native plan
expects and, when a local plan exists, lowers it to native operators over the
remote stream. The local plan's shape and its `RemoteResult` leaf are defined in
[Substrait subset](connector-contract.md#substrait-subset). The adapter
replaces the leaf with the remote read and resolves the local plan's column
references against the remote output schema.

The local plan need not be the literal remainder of the offered plan
([Connector-generated local work](query-engine-pushdown.md#connector-generated-local-work)).
If any part of it cannot be lowered, the adapter rejects the whole proposal and
frees its remote execution. Adoption is atomic: a rejected proposal changes no
native state
([Applying a result](query-engine-pushdown.md#applying-a-result)).

## Types

Each adapter holds one mapping table from Analytics types to its Query Engine's
types, with the loss of each mapping. The table covers every Analytics type the
library can return, and converting a stream schema fails when a field differs
from the planned type
([Execution](analytics-types.md#execution)). An extension-encoded type arrives
as its Arrow storage type with `ARROW:extension:name` and its metadata
([Extension metadata](analytics-types.md#extension-metadata)); the adapter
identifies it by the extension name, because the storage type alone cannot tell
`Uuid` from `Text`.

## Tests

### Plan construction

An adapter's own tests pass every plan shape it builds to `plan_pushdown`. The
library validates each offered plan before the Connector sees it, so a plan
that is not `invalid request` meets the profile; no separate validator or
profile description is maintained for adapters. A library built with the
`connector-api` feature `test-connector` includes the SDK's in-memory
Connector, which plans without a Data Source.

### Integration tests

Each adapter's integration suite runs against its pinned Query Engine version
and exercises the native planner boundary. It tests that:

- complete pushdown executes only the remote plan and replaces the offered
  native region;
- partial pushdown converts and runs the local plan over remote output;
- an unlowerable local plan rejects only that proposal and leaves native state
  unchanged;
- another complete request can still provide an adoptable proposal after such a
  rejection;
- fully-local fallback converts `RemoteResult -> Filter -> Project` and does
  not read the named table again;
- Connector-introduced intermediate columns and expressions are mapped safely;
- read, projection, and filter that appear together are offered as one complete
  region rather than independent requests;
- alternative join shapes become separate complete requests;
- the first connector scan initializes the exact base `ReadRel` before later
  operator attempts;
- unsupported results and errors leave the native plan unchanged;
- execution uses the adopted remote execution without changing its rewrite; and
- native handle persistence, if required by the adapter, is tested by the
  adapter rather than through the shared planning contract.

Engine-specific callbacks add their own cases:

| Query Engine | The integration suite proves that |
| --- | --- |
| Spark | a builder whose multi-predicate callback fails after an earlier predicate was accepted rolls back to its original state |
| Trino | predicates presented again are not appended again, and every previously retained constraint remains in Trino when a new exact constraint is added |
| DataFusion | the standard provider reports representable filters as `Inexact`, keeps planned state local to `scan`, and supplies a local projection over a full remote scan when needed |
| DuckDB | bind state and the residual vector roll back together when a multi-expression callback fails after an earlier expression was accepted |
