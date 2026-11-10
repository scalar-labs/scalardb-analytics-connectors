# Pushdown planning

## Purpose

Pushdown planning turns one complete offered Substrait region into a
semantically equivalent combination of work for the Data Source and optional
work for the Query Engine. It also generates the executable remote query.
Pushdown planning ends when it produces a `PlannedPushdown`; shipping and
executing that query happen later during pushdown execution.

~~~text
PushdownRequest {
    offered_plan: Substrait Plan
}

PlannedPushdown {
    remote_plan
    local_plan: optional
    remote: RemoteExecution { query, output_schema }
}
~~~

This is a semantic decomposition, not necessarily a structural cut. A Connector
rewrite rule may introduce intermediate operations that were not present in the
offered plan. For example, partial `AVG(x)` pushdown may produce remote
`SUM(x)` and `COUNT(x)` values and a local division.

## Glossary

| Term | Meaning |
| --- | --- |
| Pushdown | An optimization technique that moves eligible query processing to the Data Source. It is not the name of a particular plan or protocol step. |
| Pushdown lifecycle | Pushdown planning followed by pushdown execution. |
| Pushdown planning | The pre-execution process that performs query decomposition and remote query generation. Its input is a `PushdownRequest`, and its output is a `PlannedPushdown`. |
| Pushdown request | One complete offered Substrait plan submitted for pushdown planning. An Adapter may submit multiple alternative requests for the same native region. |
| Logical plan | The Connector SDK's strictly typed internal representation of the accepted Substrait semantic profile. Connector planning and rewrite rules operate on it. |
| Query decomposition | A semantics-preserving transformation of the offered query into a `remote_plan` and an optional `local_plan`. It may rewrite operators and expressions; it is not limited to cutting an existing plan at one boundary. |
| Remote plan | The complete Substrait plan whose work is assigned to the Data Source. |
| Local plan | The optional complete Substrait plan over `RemoteResult` that consumes the remote output and finishes the query in the Query Engine. It contains no independent Data Source read. |
| Remote query generation | Translation of the remote plan into a Data Source-executable query. |
| Executable query | Connector-owned representation of the generated remote query. Its type is shared by the session's planner and executor. A SQL Connector uses one complete non-parameterized SQL statement; another Connector may use another Data Source-specific type. It contains no credentials or live resources. |
| Remote execution | The executable query together with the remote output schema: everything execution needs, in the planning process or, carried as opaque bytes, in another process. |
| Planned pushdown | The result of pushdown planning: the remote plan, optional local plan, and remote execution. It remains a proposal until the Query Engine adapter adopts it. |
| Pushdown execution | Execution of an adopted planned pushdown, including query shipping, Data Source execution, and optional local-plan execution. |
| Query shipping | The runtime action of sending the generated remote query to the Data Source for execution. |

`Shippable` may be used narrowly for an expression or operator that can be sent
to a particular Data Source without changing its semantics. It is not a central
domain object: the Connector sits between the Query Engine and Data Source, so
the more neutral `Data Source-executable` wording is preferred when direction
does not matter.

## Lifecycle

~~~text
PushdownRequest
  |
  | Pushdown planning
  |   1. Query decomposition
  |   2. Remote query generation
  v
PlannedPushdown
  |
  | Pushdown execution
  |   1. Query shipping
  |   2. Data Source execution
  |   3. Optional local-plan execution
  v
Query result
~~~

Pushdown planning and pushdown execution are separate phases. Connector
planning may perform catalog or capability I/O, but it does not execute the
user's data query. Query shipping begins only after the Adapter has adopted a
planned pushdown.

## Ownership

| Component | Responsibility |
| --- | --- |
| Query Engine adapter | Finds rewritable native regions, creates complete alternative requests, chooses engine-specific join shapes, rejects proposals it cannot lower, and atomically installs an adoptable result |
| Connector SDK | Defines the common planning and execution boundary, validates and converts Substrait plans, and owns the standard `QueryPlan` construction and Connector-supplied rule application flow |
| Connector planner | Performs required catalog I/O, resolves opaque bindings, supplies source-specific rewrite rules, generates the remote executable query, returns the `PlannedPushdown`, and guarantees that executing local after remote preserves the offered semantics; a custom planner may replace the standard SDK flow |
| Connector executor | Ships and executes the generated remote executable query after the Adapter adopts the result |

The SDK does not prove semantic equivalence between the offered plan and the
remote/local result. That guarantee belongs to each finite Connector rewrite
rule and its conformance tests.

## Data model

The connector boundary and connector planning deliberately use different plan
representations:

~~~text
PushdownRequest(Substrait Plan)
  -> validate and convert
LogicalPlan
  -> decompose and rewrite
QueryPlan {
    remote_plan: LogicalPlan
    local_plan: optional LogicalPlan
}
  -> generate the remote query and convert plans to Substrait
PlannedPushdown {
    remote_plan: Substrait Plan
    local_plan: optional Substrait Plan
    remote: RemoteExecution<ConnectorQuery> { query, output_schema }
}
~~~

`LogicalPlan` is an internal IR, not another interchange format. It strictly
types the Substrait semantics accepted by the SDK so that planning code cannot
construct relation shapes already rejected at validation. Within that
supported profile, conversion to and from a validated Substrait plan is
semantically lossless: query meaning and required metadata are preserved, but
protobuf byte equality and unknown-field preservation are not promised. The
reason for separating the two representations is recorded in
[Separate interchange and planning representations](design/separate-interchange-and-planning-representations.md).

`PlannedPushdown` is the boundary domain model. Data Source-specific planning
metadata is a Connector-private, per-call value used while rewriting and generating the
executable query. It is dropped before the result is returned and is not stored
in `PlannedPushdown` or exposed through `ConnectorPlanner`.

Rewrite rules describe tree transformations and return new immutable
`LogicalPlan` values. They do not separately maintain plan-level derived state:
constructing the replacement plan derives and stores its `output_schema` from
the new tree and root names. The remote plan's schema is the intermediate
schema. When a local plan exists, its `RemoteResult` extension leaf declares
and consumes that schema. When no local plan exists, remote output is the final
result.

`RemoteExecution<Q>` is the part of the planned pushdown that execution needs:
the Connector-owned query `Q` and the remote output schema. `Q` remains a
concrete type shared by the session's planner and executor. A SQL Connector
uses `RenderedQuery`, which contains one complete non-parameterized SQL
statement whose typed literals were already rendered by its dialect. It
contains no credentials, live connections, or process-local pointers, so it
can cross a process boundary ([Execution in another process](#execution-in-another-process)).

## Execution in another process

Spark plans on the driver and reads on executors; Trino plans on the
coordinator and reads on workers. The planning process therefore hands the
reading process `RemoteExecution::to_bytes`, and the reading process restores
it with `RemoteExecution::from_bytes` and passes it to `execute_planned` on a
session it opened itself:

~~~text
planning process                       reading process
PlannedPushdown.remote                 opens its own Data Source and session
  -> to_bytes                            from its registration settings
  -> byte[] inside the Adapter's         RemoteExecution::from_bytes
     split or partition  ------------->   -> execute_planned(executor, remote, deadline)
~~~

- The SDK frames the remote output schema together with the Connector's query
  bytes. The Connector encodes and decodes only its own `Q` through
  `PortableQuery`.
- The bytes are opaque to the Adapter and are valid only for the library
  version that produced them. The planning and reading processes run the same
  library because the library and its callers are updated together
  ([Compatibility and versions](connector-contract.md#compatibility-and-versions)).
- Credentials, connections, and the local plan are not carried. The local plan
  was consumed when the Adapter adopted the proposal in the planning process.
- Neither the library version nor the Data Source is checked when bytes are
  restored. Passing bytes to another library version or another Data Source is
  a caller error. Bytes that cannot be restored are `invalid request`.

## Planning flow

~~~text
native region
  -> Adapter serializes one or more complete offered Substrait plans
  -> SDK validates each request and converts it to LogicalPlan
  -> Connector loads private metadata required for this plan
  -> SDK creates the initial complete QueryPlan
  -> SDK applies the Connector's RewriteRuleSet using that metadata
  -> Connector generates the executable query from the final remote plan and metadata
  -> SDK converts the remote/local plans to Substrait and returns PlannedPushdown
  -> Adapter attempts to lower each complete local plan and checks its mappings
  -> Adapter rejects that proposal or installs it atomically
~~~

`PlannedPushdown` is a proposal until the Adapter adopts it. Failure to lower a
local plan is not a Connector planning error and does not invalidate other
requests. An Adapter may discard that result, retain the original native plan,
or use an adoptable result from another complete request.

After adoption, a SQL Connector passes the rendered statement to the shared
ADBC query path. Execution does not receive the logical plan, planning metadata,
dialect, or semantic policy and therefore cannot repeat SQL translation. The
ADBC path remains responsible for lazy first pull, batches, cancellation,
diagnostics, and resource release.

Catalog and capability I/O is allowed during Connector planning. The required
facts are only known after the offered plan is available. Prefetching and caches
are implementation strategies, not a required `PlanningSnapshot` protocol.
The standard `RewriteRuleConnectorPlanner` keeps this control flow in the SDK;
the Connector supplies metadata loading, its ordered rule set, and executable
query generation. Because the initial `QueryPlan` is already complete, this
flow returns `Planned` on success even when every rule is a no-op. The basic
`ConnectorPlanner` remains available for Connectors whose Data Source requires
a different planning flow or an explicit `Unsupported` result.

## Execution flow

After adoption, execution uses the result of planning without decomposing the
query again:

~~~text
PlannedPushdown
  -> Connector ships remote.query to the Data Source
  -> Data Source returns the remote result
  -> if local_plan exists, Query Engine executes it over the remote result
  -> Query Engine returns the final result
~~~

When `local_plan` is absent, the remote result is the final result. When it is
present, the remote plan's output schema is the local plan's input schema.

## Rewrite rule selection

Connector-owned `RewriteRule` values are semantics-preserving converters over
a complete remote/local query decomposition. Each rule receives a valid
decomposition and returns another valid decomposition. Rules may therefore be
composed without creating incomplete intermediate plans.

A rule that cannot improve the current decomposition is a no-op. It does not
terminate the rule sequence or require a special fully-local result type. The
initial decomposition keeps all relational processing in the local plan while
the remote plan performs the minimum Data Source read. Subsequent rules move or
rewrite eligible work for remote execution. For the initial
Read/Filter/Projection subset, that state is equivalent to:

~~~text
remote_plan: Read(all base columns)

local_plan:
  Project
    Filter
      RemoteResult
~~~

This provides a complete plan before any source-specific pushdown rule exists.
Adding a rule changes the placement or representation of work, not the planning
result shape.

## Complete and partial pushdown

Complete pushdown has no local plan:

~~~text
PlannedPushdown {
    remote_plan
    local_plan: None
}
~~~

Partial pushdown has a local plan:

~~~text
PlannedPushdown {
    remote_plan
    local_plan: Some(plan over RemoteResult)
}
~~~

There is no separate `Exact`, `RequiresResidual`, or `PushdownCoverage` field.
The two plans directly describe the execution placement.

## Semantic rewrite example

An offered aggregate may contain:

~~~text
Aggregate(avg(value))
  Read
~~~

A Connector rule may produce:

~~~text
remote_plan:
  Aggregate(sum(value), count(value))
    Read

local_plan:
  Project(remote_sum / remote_count)
    RemoteResult
~~~

If remote execution can return multiple partial groups, the local plan instead
merges partial sums and counts before division. The rule is responsible for
NULL, empty-input, grouping, overflow, decimal, and floating-point semantics.

The remote plan is therefore not required to be a subset of the offered plan.
Resolving physical objects and proving that the generated query implements the
remote plan are Connector responsibilities.

## Boundary checks

The SDK validates only the externally offered Substrait plan. After rewrite
rules produce internal remote and local `LogicalPlan` values, constructing
`PlannedPushdown` asserts the decomposition contract:

- the remote plan terminates in one table read;
- a local plan, when present, terminates in one `RemoteResult` whose schema
  equals the remote output schema;
- the local output schema equals the offered output schema, or the remote
  output schema equals it when no local plan is present.

These assertions check Connector-produced values, so a violation is a
Connector implementation bug rather than invalid external input. They do not
prove that a rewrite is semantically equivalent to the offered plan.

`PushdownRequest` carries an already decoded Substrait `Plan`. Serialization,
decoding, and serialized-size limits belong to a concrete transport or
persistence boundary rather than the planning API.

`RemoteExecution<Q>` is generated by the trusted Connector. Its query type
implements `PortableQuery`, so only the Connector that owns `Q` encodes and
decodes it ([Execution in another process](#execution-in-another-process)). The
SDK imposes no shared size limit on `Q`.

Boundary checks do not create a second domain result such as a
`ConnectorPlanningResult` that is converted into `PlannedPushdown`; they inspect
the same planned pushdown value. Likewise, a fingerprint is not part of the
pushdown-planning model by default. If a concrete cache, persistence, or
distributed-restore requirement later needs stable identity, that requirement
must define what is fingerprinted and when it is checked.

Semantic equivalence is tested per Connector rewrite rule. It is not rechecked
by the SDK at execution time.

## Current subset

The common offered validator currently accepts one fixed logical shape:
`RelRoot -> ProjectRel? -> FilterRel? -> ReadRel`. The terminal read is a bare
named-table scan. A rewrite ships filtering or projection by moving its
`Filter` or `Project` node into the remote plan; it does not encode that
decision in Substrait's scan-local `ReadRel.filter` or
`ReadRel.projection` fields.

This distinction is intentional. Substrait's read fields express operations
already embedded in a scan. Pushdown planning instead performs query
decomposition: it decides whether each logical operator belongs to the remote
or local plan. Keeping those operators explicit makes that placement visible
and avoids two internal representations for the same filter or projection.
The Connector's remote query generator later translates the complete remote
operator tree into SQL or another Data Source-specific query.

The initial `QueryPlan` contains
`Project? -> Filter? -> RemoteResult` as a `LogicalPlan`. Converting that plan
to the boundary representation produces
`RelRoot -> ProjectRel? -> FilterRel? -> ExtensionLeafRel(RemoteResult)`.
Before this boundary conversion, `PlannedPushdown` construction asserts the
remote-to-local schema and final offered output schema on the internal plans.
An unchanged decomposition is a no-op rather than a distinct planning result.
The remote/local model does not encode this initial limitation in
`ConnectorPlanner`; adding aggregate, join, and other rules adds explicitly
ordered logical shapes without changing the planning result type.
